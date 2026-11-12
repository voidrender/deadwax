use crate::app::{App, AppEvent, Effect};
use crate::player::{EngineEvent, PlayerCommand, PlayerSnapshot};
use crate::subsonic::SubsonicClient;
use crate::ui;
use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::Backend;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::time::MissedTickBehavior;

const TICK: Duration = Duration::from_millis(250);
/// How long quitting waits for the engine to stop mpv.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Everything the runtime loop and effect executor need.
pub struct Ctx {
    /// `None` until a server is configured and logged in.
    pub client: Option<SubsonicClient>,
    pub player: mpsc::UnboundedSender<PlayerCommand>,
    /// Spawned work reports back through this sender.
    pub tx: mpsc::UnboundedSender<AppEvent>,
    pub rx: mpsc::UnboundedReceiver<AppEvent>,
    pub snapshot: watch::Receiver<PlayerSnapshot>,
    pub engine_events: mpsc::UnboundedReceiver<EngineEvent>,
}

impl Ctx {
    pub fn new(
        client: Option<SubsonicClient>,
        player: mpsc::UnboundedSender<PlayerCommand>,
        snapshot: watch::Receiver<PlayerSnapshot>,
        engine_events: mpsc::UnboundedReceiver<EngineEvent>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            client,
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
    let result = event_loop(terminal, app, &mut ctx).await;
    shutdown_engine(&mut ctx).await;
    result
}

async fn event_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    mut app: App,
    ctx: &mut Ctx,
) -> anyhow::Result<()>
where
    B::Error: Send + Sync + 'static,
{
    let mut input = EventStream::new();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut snapshot_open = true;
    let mut engine_open = true;

    app.player = ctx.snapshot.borrow_and_update().clone();
    terminal.draw(|f| ui::draw(f, &mut app))?;

    while !app.should_quit {
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
        for effect in app.handle_event(ev) {
            execute(effect, &mut app, ctx);
        }
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
    }
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

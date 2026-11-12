use crate::app::{App, AppEvent, Effect};
use crate::player::{EngineEvent, PlayerCommand, PlayerSnapshot};
use crate::subsonic::SubsonicClient;
use crate::ui;
use crossterm::event::{Event, EventStream};
use futures::{Stream, StreamExt};
use ratatui::Terminal;
use ratatui::backend::Backend;
use std::sync::atomic::{AtomicBool, Ordering};
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    async fn run_loop_briefly() -> anyhow::Result<()> {
        let (player, _player_rx) = mpsc::unbounded_channel();
        let (_snap_tx, snapshot) = watch::channel(PlayerSnapshot::default());
        let (_events_tx, engine_events) = mpsc::unbounded_channel();
        let mut ctx = Ctx::new(None, player, snapshot, engine_events);
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

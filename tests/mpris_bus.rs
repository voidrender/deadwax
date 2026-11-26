//! The MPRIS service on a private session bus.

#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use deadwax::integrations::mpris::{self, CoverLookup, Mpris};
use deadwax::player::{EngineEvent, PlayerCommand};
use deadwax::subsonic::Song;
use futures::StreamExt;
use tokio::sync::mpsc;
use zbus::OwnedVariant;

const WAIT: Duration = Duration::from_secs(5);
const DEST: &str = "org.mpris.MediaPlayer2.deadwax";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

/// A private bus, killed on drop.
struct Bus {
    daemon: Child,
    address: String,
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// Starts a private bus, or `None` when `dbus-daemon` is missing and
/// `DEADWAX_REQUIRE_DBUS` is not set. CI sets it so these tests cannot
/// pass by skipping.
fn bus() -> Option<Bus> {
    let spawned = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let mut daemon = match spawned {
        Ok(d) => d,
        Err(e) => {
            assert!(
                std::env::var_os("DEADWAX_REQUIRE_DBUS").is_none(),
                "DEADWAX_REQUIRE_DBUS is set but dbus-daemon failed: {e}"
            );
            eprintln!("skipping: dbus-daemon unavailable ({e})");
            return None;
        }
    };
    let mut address = String::new();
    BufReader::new(daemon.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    Some(Bus {
        daemon,
        address: address.trim().to_string(),
    })
}

fn song() -> Song {
    serde_json::from_value(serde_json::json!({
        "id": "s1", "title": "Title", "artist": "Artist", "album": "Album",
        "coverArt": "c1", "duration": 200,
    }))
    .unwrap()
}

fn no_cover() -> CoverLookup {
    Arc::new(|_| Box::pin(async { None }))
}

struct Running {
    _bus: Bus,
    mpris: Mpris,
    commands: mpsc::UnboundedReceiver<PlayerCommand>,
    client: zbus::Connection,
}

async fn start(cover: CoverLookup) -> Option<Running> {
    let bus = bus()?;
    let (player, commands) = mpsc::unbounded_channel();
    let builder = zbus::connection::Builder::address(bus.address.as_str());
    let mpris = mpris::spawn(builder, player, cover)
        .await
        .expect("service starts");
    let client = zbus::connection::Builder::address(bus.address.as_str())
        .build()
        .await
        .unwrap();
    Some(Running {
        _bus: bus,
        mpris,
        commands,
        client,
    })
}

async fn proxy<'a>(c: &'a zbus::Connection, iface: &'a str) -> zbus::Proxy<'a> {
    zbus::proxy::Builder::new(c)
        .destination(DEST)
        .path(PATH)
        .interface(iface)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .unwrap()
}

type Meta = HashMap<String, OwnedVariant>;

/// Polls the player's metadata until `done` holds for it.
async fn metadata_until(p: &zbus::Proxy<'_>, done: impl Fn(&Meta) -> bool) -> Meta {
    tokio::time::timeout(WAIT, async {
        loop {
            let m: Meta = p.get_property("Metadata").await.unwrap();
            if done(&m) {
                return m;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("metadata never matched")
}

fn string(m: &Meta, key: &str) -> Option<String> {
    m.get(key)
        .map(|v| String::try_from(v.try_clone().unwrap()).unwrap())
}

#[tokio::test]
async fn publishes_identity_and_track() {
    let Some(r) = start(no_cover()).await else {
        return;
    };
    let root = proxy(&r.client, "org.mpris.MediaPlayer2").await;
    let identity: String = root.get_property("Identity").await.unwrap();
    assert_eq!(identity, "Deadwax");
    let entry: String = root.get_property("DesktopEntry").await.unwrap();
    assert_eq!(entry, "deadwax");
    assert!(!root.get_property::<bool>("CanQuit").await.unwrap());
    assert!(!root.get_property::<bool>("CanRaise").await.unwrap());

    let player = proxy(&r.client, PLAYER).await;
    let status: String = player.get_property("PlaybackStatus").await.unwrap();
    assert_eq!(status, "Stopped");
    assert!(!player.get_property::<bool>("CanPlay").await.unwrap());

    r.mpris
        .events
        .send(EngineEvent::TrackStarted(Box::new(song())))
        .unwrap();
    let m = metadata_until(&player, |m| m.contains_key("xesam:title")).await;
    assert_eq!(string(&m, "xesam:title").as_deref(), Some("Title"));
    assert_eq!(string(&m, "xesam:album").as_deref(), Some("Album"));
    let artists = Vec::<String>::try_from(m["xesam:artist"].try_clone().unwrap()).unwrap();
    assert_eq!(artists, ["Artist"]);
    assert_eq!(
        i64::try_from(m["mpris:length"].try_clone().unwrap()).unwrap(),
        200_000_000
    );
    assert!(!m.contains_key("xesam:url"), "{m:?}");
    let status: String = player.get_property("PlaybackStatus").await.unwrap();
    assert_eq!(status, "Playing");
    assert!(player.get_property::<bool>("CanGoNext").await.unwrap());
    r.mpris.stop().await;
}

#[tokio::test]
async fn cover_arrives_as_art_url() {
    let cover: CoverLookup = Arc::new(|id| {
        assert_eq!(id, "c1");
        Box::pin(async { Some(PathBuf::from("/tmp/a b.img")) })
    });
    let Some(r) = start(cover).await else {
        return;
    };
    r.mpris
        .events
        .send(EngineEvent::TrackStarted(Box::new(song())))
        .unwrap();
    let player = proxy(&r.client, PLAYER).await;
    let m = metadata_until(&player, |m| m.contains_key("mpris:artUrl")).await;
    assert_eq!(
        string(&m, "mpris:artUrl").as_deref(),
        Some("file:///tmp/a%20b.img")
    );
    r.mpris.stop().await;
}

#[tokio::test]
async fn play_pause_reaches_the_engine() {
    let Some(mut r) = start(no_cover()).await else {
        return;
    };
    r.mpris
        .events
        .send(EngineEvent::TrackStarted(Box::new(song())))
        .unwrap();
    let player = proxy(&r.client, PLAYER).await;
    metadata_until(&player, |m| m.contains_key("xesam:title")).await;
    let () = player.call("PlayPause", &()).await.unwrap();
    let () = player.call("Seek", &(-5_000_000_i64)).await.unwrap();
    let track = zbus::ObjectPath::try_from("/org/deadwax/track/1").unwrap();
    let () = player
        .call("SetPosition", &(track, 30_000_000_i64))
        .await
        .unwrap();
    let got = tokio::time::timeout(WAIT, async {
        [
            r.commands.recv().await.unwrap(),
            r.commands.recv().await.unwrap(),
            r.commands.recv().await.unwrap(),
        ]
    })
    .await
    .unwrap();
    assert_eq!(
        got,
        [
            PlayerCommand::TogglePause,
            PlayerCommand::SeekRelative(-5.0),
            PlayerCommand::SeekTo(30.0),
        ]
    );
    r.mpris.stop().await;
}

#[tokio::test]
async fn seeked_is_signalled() {
    let Some(r) = start(no_cover()).await else {
        return;
    };
    let player = proxy(&r.client, PLAYER).await;
    let mut seeked = player.receive_signal("Seeked").await.unwrap();
    r.mpris
        .events
        .send(EngineEvent::TrackStarted(Box::new(song())))
        .unwrap();
    r.mpris.events.send(EngineEvent::Seeked(12.0)).unwrap();
    let msg = tokio::time::timeout(WAIT, seeked.next())
        .await
        .unwrap()
        .unwrap();
    let pos: i64 = msg.body().deserialize().unwrap();
    assert_eq!(pos, 12_000_000);
    let now: i64 = player.get_property("Position").await.unwrap();
    assert_eq!(now, 12_000_000);
    r.mpris.stop().await;
}

#[tokio::test]
async fn stopping_releases_the_name() {
    let Some(r) = start(no_cover()).await else {
        return;
    };
    let dbus = zbus::fdo::DBusProxy::new(&r.client).await.unwrap();
    let name = DEST.try_into().unwrap();
    assert!(dbus.name_has_owner(name).await.unwrap());
    r.mpris.stop().await;
    let name = DEST.try_into().unwrap();
    assert!(!dbus.name_has_owner(name).await.unwrap());
}

#[tokio::test]
async fn no_bus_means_no_service() {
    let (player, _commands) = mpsc::unbounded_channel();
    let builder = zbus::connection::Builder::address("unix:path=/nonexistent/deadwax-bus");
    assert!(mpris::spawn(builder, player, no_cover()).await.is_none());
}

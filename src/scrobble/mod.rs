//! Scrobble tracking (listened-time threshold) and the offline outbox.

pub mod outbox;
pub mod tracker;

pub use outbox::{Outbox, PendingScrobble};
pub use tracker::{ScrobbleAction, ScrobbleTracker, threshold_secs};

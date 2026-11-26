//! Playback on the session bus as an MPRIS player, so media keys and
//! desktop media widgets can see and control Deadwax.

pub mod control;
mod service;
pub mod state;

pub use service::{BUS_NAME, CoverLookup, Mpris, session_bus, spawn};

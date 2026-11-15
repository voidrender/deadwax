//! Cover art: a private on-disk cache of Subsonic art and a public lookup for Discord.

pub mod cover_cache;
pub mod public;

pub use cover_cache::CoverCache;
pub use public::{ArtQuery, Endpoints, PublicArtResolver, normalize};

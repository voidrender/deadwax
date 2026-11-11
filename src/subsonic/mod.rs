pub(crate) mod auth;
mod client;
mod error;
mod models;
mod playlists;

pub use auth::Auth;
pub use client::SubsonicClient;
pub use error::SubsonicError;
pub use models::{Album, AlbumWithSongs, Artist, ArtistWithAlbums, SearchResults, Song};
pub use playlists::{Playlist, PlaylistWithSongs};

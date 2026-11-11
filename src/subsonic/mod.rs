mod auth;
mod client;
mod error;
mod models;

pub use auth::Auth;
pub use client::SubsonicClient;
pub use error::SubsonicError;
pub use models::{Album, AlbumWithSongs, Artist, ArtistWithAlbums, SearchResults, Song};

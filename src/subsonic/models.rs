use serde::{Deserialize, Deserializer, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artist {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub album_count: u32,
    pub cover_art: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Album {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub artist: String,
    pub artist_id: Option<String>,
    pub year: Option<u32>,
    #[serde(default)]
    pub song_count: u32,
    #[serde(default)]
    pub duration: u32,
    pub cover_art: Option<String>,
    pub music_brainz_id: Option<String>,
    pub genre: Option<String>,
    pub play_count: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Song {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub artist: String,
    pub artist_id: Option<String>,
    #[serde(default)]
    pub album: String,
    pub album_id: Option<String>,
    pub track: Option<u32>,
    pub year: Option<u32>,
    #[serde(default)]
    pub duration: u32,
    pub cover_art: Option<String>,
    pub suffix: Option<String>,
    pub bit_rate: Option<u32>,
    pub disc_number: Option<u32>,
    pub genre: Option<String>,
    pub play_count: Option<u64>,
    /// File size in bytes.
    pub size: Option<u64>,
    /// In Hz. Sent by `OpenSubsonic` servers such as Navidrome.
    pub sampling_rate: Option<u32>,
    /// Sent by `OpenSubsonic` servers for lossless files.
    pub bit_depth: Option<u32>,
    /// The server sends a timestamp when starred and omits the field
    /// otherwise. Saved queue state stores a plain bool.
    #[serde(default, deserialize_with = "present_as_true")]
    pub starred: bool,
}

fn present_as_true<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Starred {
        Flag(bool),
        Stamp(serde::de::IgnoredAny),
    }
    Ok(match Option::<Starred>::deserialize(d)? {
        Some(Starred::Flag(b)) => b,
        Some(Starred::Stamp(_)) => true,
        None => false,
    })
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistWithAlbums {
    #[serde(flatten)]
    pub artist: Artist,
    #[serde(default, rename = "album")]
    pub albums: Vec<Album>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumWithSongs {
    #[serde(flatten)]
    pub album: Album,
    #[serde(default, rename = "song")]
    pub songs: Vec<Song>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    #[serde(default, rename = "artist")]
    pub artists: Vec<Artist>,
    #[serde(default, rename = "album")]
    pub albums: Vec<Album>,
    #[serde(default, rename = "song")]
    pub songs: Vec<Song>,
}

/// Wire shape of `getArtists`.
#[derive(Deserialize)]
pub(super) struct ArtistIndexes {
    #[serde(default)]
    pub index: Vec<ArtistIndex>,
}

#[derive(Deserialize)]
pub(super) struct ArtistIndex {
    #[serde(default)]
    pub artist: Vec<Artist>,
}

/// Wire shape of `getAlbumList2`.
#[derive(Deserialize)]
pub(super) struct AlbumList {
    #[serde(default)]
    pub album: Vec<Album>,
}

/// Wire shape of `getRandomSongs` and `getStarred2`.
#[derive(Deserialize)]
pub(super) struct SongList {
    #[serde(default)]
    pub song: Vec<Song>,
}

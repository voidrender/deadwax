//! Sort orders for the library and playlist lists, and the file that keeps
//! them between runs.

use super::library::ArtistRow;
use crate::subsonic::{Album, Playlist, Song};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtistSort {
    /// The server's order, which already ignores leading articles.
    #[default]
    Name,
    MostAlbums,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlbumSort {
    /// Release year, newest first.
    #[default]
    Newest,
    Oldest,
    Name,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackSort {
    /// The server's order: disc and track for an album, starred order for
    /// Favorites.
    #[default]
    Album,
    Title,
    Artist,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistSort {
    #[default]
    Server,
    Name,
}

impl ArtistSort {
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Name => Self::MostAlbums,
            Self::MostAlbums => Self::Name,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::MostAlbums => "most albums",
        }
    }
}

impl AlbumSort {
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Newest => Self::Oldest,
            Self::Oldest => Self::Name,
            Self::Name => Self::Newest,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Newest => "newest",
            Self::Oldest => "oldest",
            Self::Name => "name",
        }
    }
}

impl TrackSort {
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Album => Self::Title,
            Self::Title => Self::Artist,
            Self::Artist => Self::Album,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Album => "album order",
            Self::Title => "title",
            Self::Artist => "artist",
        }
    }
}

impl PlaylistSort {
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Server => Self::Name,
            Self::Name => Self::Server,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Server => "server order",
            Self::Name => "name",
        }
    }
}

/// Every sort choice, saved to `sorts.json` in the data directory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sorts {
    pub artists: ArtistSort,
    pub albums: AlbumSort,
    pub tracks: TrackSort,
    pub playlists: PlaylistSort,
}

impl Sorts {
    /// Reads saved sorts. A missing or unreadable file gives the defaults.
    pub fn load(path: &Path) -> Sorts {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!("ignoring unreadable {}: {e}", path.display());
                Sorts::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Sorts::default(),
            Err(e) => {
                tracing::warn!("could not read {}: {e}", path.display());
                Sorts::default()
            }
        }
    }

    /// Writes through a temp file and a rename, like the queue state.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).context("creating the data directory")?;
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, serde_json::to_vec(self)?).context("writing sort orders")?;
        std::fs::rename(&tmp, path).context("replacing sort orders")?;
        Ok(())
    }
}

/// Each id's position in the order the server sent, so a sort can fall back
/// to that order.
pub(crate) fn rank<'a>(ids: impl Iterator<Item = &'a str>) -> HashMap<String, usize> {
    ids.enumerate().map(|(i, id)| (id.to_string(), i)).collect()
}

fn rank_of(ranks: &HashMap<String, usize>, id: &str) -> usize {
    ranks.get(id).copied().unwrap_or(usize::MAX)
}

fn by_text(a: &str, b: &str) -> Ordering {
    a.to_lowercase().cmp(&b.to_lowercase())
}

/// Sorts artist rows. Favorites stays first whatever the order.
pub(crate) fn sort_artists(
    rows: &mut [ArtistRow],
    sort: ArtistSort,
    ranks: &HashMap<String, usize>,
) {
    rows.sort_by(|a, b| match (a, b) {
        (ArtistRow::Favorites, ArtistRow::Favorites) => Ordering::Equal,
        (ArtistRow::Favorites, _) => Ordering::Less,
        (_, ArtistRow::Favorites) => Ordering::Greater,
        (ArtistRow::Artist(a), ArtistRow::Artist(b)) => {
            let by_rank = rank_of(ranks, &a.id).cmp(&rank_of(ranks, &b.id));
            match sort {
                ArtistSort::Name => by_rank,
                ArtistSort::MostAlbums => b.album_count.cmp(&a.album_count).then(by_rank),
            }
        }
    });
}

/// Sorts albums. Albums without a year go last in both year orders.
pub(crate) fn sort_albums(albums: &mut [Album], sort: AlbumSort) {
    albums.sort_by(|a, b| {
        let by_name = by_text(&a.name, &b.name).then_with(|| a.id.cmp(&b.id));
        let by_year = match (a.year, b.year) {
            (Some(x), Some(y)) if sort == AlbumSort::Newest => y.cmp(&x),
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        };
        match sort {
            AlbumSort::Newest | AlbumSort::Oldest => by_year.then(by_name),
            AlbumSort::Name => by_name,
        }
    });
}

/// Sorts songs, falling back to the server's order for ties.
pub(crate) fn sort_tracks(songs: &mut [Song], sort: TrackSort, ranks: &HashMap<String, usize>) {
    songs.sort_by(|a, b| {
        let by_rank = rank_of(ranks, &a.id).cmp(&rank_of(ranks, &b.id));
        match sort {
            TrackSort::Album => by_rank,
            TrackSort::Title => by_text(&a.title, &b.title).then(by_rank),
            TrackSort::Artist => by_text(&a.artist, &b.artist).then(by_rank),
        }
    });
}

/// Sorts the playlist list, falling back to the server's order for ties.
pub(crate) fn sort_playlists(
    lists: &mut [Playlist],
    sort: PlaylistSort,
    ranks: &HashMap<String, usize>,
) {
    lists.sort_by(|a, b| {
        let by_rank = rank_of(ranks, &a.id).cmp(&rank_of(ranks, &b.id));
        match sort {
            PlaylistSort::Server => by_rank,
            PlaylistSort::Name => by_text(&a.name, &b.name).then(by_rank),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subsonic::Artist;

    fn album(id: &str, name: &str, year: Option<u32>) -> Album {
        Album {
            id: id.into(),
            name: name.into(),
            artist: String::new(),
            artist_id: None,
            year,
            song_count: 0,
            duration: 0,
            cover_art: None,
            music_brainz_id: None,
            ..Default::default()
        }
    }

    fn ids<T>(items: &[T], id: impl Fn(&T) -> &str) -> Vec<String> {
        items.iter().map(|i| id(i).to_string()).collect()
    }

    #[test]
    fn albums_default_to_newest_with_unknown_years_last() {
        let mut a = vec![
            album("a", "Alpha", Some(2001)),
            album("b", "beta", None),
            album("c", "Gamma", Some(2020)),
            album("d", "Delta", Some(2020)),
        ];
        sort_albums(&mut a, Sorts::default().albums);
        assert_eq!(ids(&a, |x| &x.id), ["d", "c", "a", "b"]);
        sort_albums(&mut a, AlbumSort::Oldest);
        assert_eq!(ids(&a, |x| &x.id), ["a", "d", "c", "b"]);
        sort_albums(&mut a, AlbumSort::Name);
        assert_eq!(ids(&a, |x| &x.id), ["a", "b", "d", "c"]);
    }

    fn artist(id: &str, albums: u32) -> ArtistRow {
        ArtistRow::Artist(Artist {
            id: id.into(),
            name: id.into(),
            album_count: albums,
            cover_art: None,
        })
    }

    #[test]
    fn artists_keep_favorites_first_and_restore_server_order() {
        let mut rows = vec![
            ArtistRow::Favorites,
            artist("the-b", 1),
            artist("c", 5),
            artist("a", 5),
        ];
        let ranks = rank(rows.iter().map(ArtistRow::id));
        sort_artists(&mut rows, ArtistSort::MostAlbums, &ranks);
        assert_eq!(ids(&rows, ArtistRow::id)[1..], ["c", "a", "the-b"]);
        assert_eq!(rows[0], ArtistRow::Favorites);
        sort_artists(&mut rows, ArtistSort::Name, &ranks);
        assert_eq!(ids(&rows, ArtistRow::id)[1..], ["the-b", "c", "a"]);
    }

    fn song(id: &str, title: &str, artist: &str) -> Song {
        Song {
            id: id.into(),
            title: title.into(),
            artist: artist.into(),
            artist_id: None,
            album: String::new(),
            album_id: None,
            track: None,
            year: None,
            duration: 0,
            cover_art: None,
            suffix: None,
            bit_rate: None,
            starred: false,
            ..Default::default()
        }
    }

    #[test]
    fn tracks_sort_and_return_to_album_order() {
        let mut s = vec![
            song("1", "zeta", "B"),
            song("2", "Alpha", "a"),
            song("3", "mid", "b"),
        ];
        let ranks = rank(s.iter().map(|x| x.id.as_str()));
        sort_tracks(&mut s, TrackSort::Title, &ranks);
        assert_eq!(ids(&s, |x| &x.id), ["2", "3", "1"]);
        sort_tracks(&mut s, TrackSort::Artist, &ranks);
        assert_eq!(ids(&s, |x| &x.id), ["2", "1", "3"]);
        sort_tracks(&mut s, TrackSort::Album, &ranks);
        assert_eq!(ids(&s, |x| &x.id), ["1", "2", "3"]);
    }

    #[test]
    fn every_cycle_returns_to_the_default() {
        let mut a = ArtistSort::default();
        let mut b = AlbumSort::default();
        let mut t = TrackSort::default();
        let mut p = PlaylistSort::default();
        for _ in 0..6 {
            a = a.next();
            b = b.next();
            t = t.next();
            p = p.next();
        }
        assert_eq!(
            (a, b, t, p),
            (
                ArtistSort::default(),
                AlbumSort::default(),
                TrackSort::default(),
                PlaylistSort::default()
            )
        );
    }

    #[test]
    fn sorts_round_trip_and_survive_bad_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sorts.json");
        assert_eq!(Sorts::load(&path), Sorts::default());
        let s = Sorts {
            artists: ArtistSort::MostAlbums,
            albums: AlbumSort::Name,
            tracks: TrackSort::Title,
            playlists: PlaylistSort::Name,
        };
        s.save(&path).unwrap();
        assert_eq!(Sorts::load(&path), s);
        std::fs::write(&path, b"{\"albums\":\"oldest\"}").unwrap();
        assert_eq!(Sorts::load(&path).albums, AlbumSort::Oldest);
        std::fs::write(&path, b"not json").unwrap();
        assert_eq!(Sorts::load(&path), Sorts::default());
    }
}

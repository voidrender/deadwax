use serde::Deserialize;

use super::{Song, SubsonicClient, SubsonicError};

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub song_count: u32,
    #[serde(default)]
    pub duration: u32,
    pub owner: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistWithSongs {
    #[serde(flatten)]
    pub playlist: Playlist,
    #[serde(default, rename = "entry")]
    pub songs: Vec<Song>,
}

/// Wire shape of `getPlaylists`.
#[derive(Deserialize)]
struct PlaylistList {
    #[serde(default)]
    playlist: Vec<Playlist>,
}

fn repeated(key: &str, values: impl IntoIterator<Item = String>) -> Vec<(&str, String)> {
    values.into_iter().map(|v| (key, v)).collect()
}

impl SubsonicClient {
    pub async fn get_playlists(&self) -> Result<Vec<Playlist>, SubsonicError> {
        let list: PlaylistList = self.call("getPlaylists", &[], "playlists").await?;
        Ok(list.playlist)
    }

    pub async fn get_playlist(&self, id: &str) -> Result<PlaylistWithSongs, SubsonicError> {
        self.call("getPlaylist", &[("id", id.to_string())], "playlist")
            .await
    }

    pub async fn create_playlist(
        &self,
        name: &str,
        song_ids: &[String],
    ) -> Result<Playlist, SubsonicError> {
        let mut params = vec![("name", name.to_string())];
        params.extend(repeated("songId", song_ids.iter().cloned()));
        self.call("createPlaylist", &params, "playlist").await
    }

    pub async fn rename_playlist(&self, id: &str, name: &str) -> Result<(), SubsonicError> {
        self.call_unit(
            "updatePlaylist",
            &[("playlistId", id.to_string()), ("name", name.to_string())],
        )
        .await
    }

    pub async fn add_to_playlist(
        &self,
        id: &str,
        song_ids: &[String],
    ) -> Result<(), SubsonicError> {
        let mut params = vec![("playlistId", id.to_string())];
        params.extend(repeated("songIdToAdd", song_ids.iter().cloned()));
        self.call_unit("updatePlaylist", &params).await
    }

    pub async fn remove_from_playlist(
        &self,
        id: &str,
        indices: &[usize],
    ) -> Result<(), SubsonicError> {
        let mut params = vec![("playlistId", id.to_string())];
        params.extend(repeated(
            "songIndexToRemove",
            indices.iter().map(usize::to_string),
        ));
        self.call_unit("updatePlaylist", &params).await
    }

    /// Replaces the contents of a playlist, which is how reordering is done.
    pub async fn replace_playlist_songs(
        &self,
        id: &str,
        song_ids: &[String],
    ) -> Result<(), SubsonicError> {
        let mut params = vec![("playlistId", id.to_string())];
        params.extend(repeated("songId", song_ids.iter().cloned()));
        self.call_unit("createPlaylist", &params).await
    }

    pub async fn delete_playlist(&self, id: &str) -> Result<(), SubsonicError> {
        self.call_unit("deletePlaylist", &[("id", id.to_string())])
            .await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::super::Auth;
    use super::*;

    fn ok() -> ResponseTemplate {
        ResponseTemplate::new(200)
            .set_body_json(json!({"subsonic-response":{"status":"ok","version":"1.16.1"}}))
    }

    fn fixture(body: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(body.to_string(), "application/json")
    }

    async fn client_for(server: &MockServer) -> SubsonicClient {
        SubsonicClient::new(&server.uri(), "alice", Auth::Password("p".into())).unwrap()
    }

    async fn values(server: &MockServer, key: &str) -> Vec<String> {
        let req = &server.received_requests().await.unwrap()[0];
        req.url
            .query_pairs()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
            .collect()
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[tokio::test]
    async fn create_sends_name_and_songs() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/createPlaylist"))
            .and(query_param("name", "Mix"))
            .respond_with(fixture(include_str!(
                "../../tests/fixtures/get_playlist.json"
            )))
            .mount(&server)
            .await;
        let p = client_for(&server)
            .await
            .create_playlist("Mix", &ids(&["a", "b"]))
            .await
            .unwrap();
        assert_eq!(p.id, "pl-1");
        assert_eq!(values(&server, "name").await, ["Mix"]);
        assert_eq!(values(&server, "songId").await, ["a", "b"]);
    }

    #[tokio::test]
    async fn replace_reorders_via_create() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/createPlaylist"))
            .and(query_param("playlistId", "pl-1"))
            .respond_with(ok())
            .expect(1)
            .mount(&server)
            .await;
        client_for(&server)
            .await
            .replace_playlist_songs("pl-1", &ids(&["c", "a", "b"]))
            .await
            .unwrap();
        assert_eq!(values(&server, "songId").await, ["c", "a", "b"]);
    }

    #[tokio::test]
    async fn remove_sends_each_index() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/updatePlaylist"))
            .and(query_param("playlistId", "pl-1"))
            .respond_with(ok())
            .expect(1)
            .mount(&server)
            .await;
        client_for(&server)
            .await
            .remove_from_playlist("pl-1", &[0, 3])
            .await
            .unwrap();
        assert_eq!(values(&server, "songIndexToRemove").await, ["0", "3"]);
    }

    #[tokio::test]
    async fn add_sends_each_song() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/updatePlaylist"))
            .and(query_param("playlistId", "pl-1"))
            .respond_with(ok())
            .expect(1)
            .mount(&server)
            .await;
        client_for(&server)
            .await
            .add_to_playlist("pl-1", &ids(&["a", "b"]))
            .await
            .unwrap();
        assert_eq!(values(&server, "songIdToAdd").await, ["a", "b"]);
    }

    #[tokio::test]
    async fn rename_and_delete_send_ids() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/updatePlaylist"))
            .and(query_param("playlistId", "pl-1"))
            .and(query_param("name", "New"))
            .respond_with(ok())
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/rest/deletePlaylist"))
            .and(query_param("id", "pl-1"))
            .respond_with(ok())
            .expect(1)
            .mount(&server)
            .await;
        let c = client_for(&server).await;
        c.rename_playlist("pl-1", "New").await.unwrap();
        c.delete_playlist("pl-1").await.unwrap();
    }

    #[tokio::test]
    async fn get_playlists_parses_list() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/getPlaylists"))
            .respond_with(fixture(include_str!(
                "../../tests/fixtures/get_playlists.json"
            )))
            .mount(&server)
            .await;
        let lists = client_for(&server).await.get_playlists().await.unwrap();
        assert_eq!(lists.len(), 2);
        assert_eq!(lists[0].song_count, 12);
        assert_eq!(lists[0].owner.as_deref(), Some("alice"));
        assert_eq!(lists[1].owner, None);
    }

    #[tokio::test]
    async fn empty_playlists_object_is_empty_vec() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/getPlaylists"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"subsonic-response":{"status":"ok","playlists":{}}})),
            )
            .mount(&server)
            .await;
        assert!(
            client_for(&server)
                .await
                .get_playlists()
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn get_playlist_reads_entry_songs() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/getPlaylist"))
            .and(query_param("id", "pl-1"))
            .respond_with(fixture(include_str!(
                "../../tests/fixtures/get_playlist.json"
            )))
            .mount(&server)
            .await;
        let p = client_for(&server)
            .await
            .get_playlist("pl-1")
            .await
            .unwrap();
        assert_eq!(p.playlist.name, "Favorites Mix");
        assert_eq!(p.songs.len(), 2);
        assert!(p.songs[1].starred);
    }
}

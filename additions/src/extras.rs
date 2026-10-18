//! Requests Spotify's own apps make that the Web API has no endpoint for:
//! an artist's monthly listeners, a song's credits, and adding local files
//! to a playlist.
//!
//! All three use the playback session, as Spotifast's other private reads
//! do (see `session_reads`), and can stop working when Spotify changes them
//! without notice. Each failure is reported, never retried through another
//! grant.

use std::sync::Arc;
use std::time::Duration;

use librespot_core::{Session, spotify_id::SpotifyId};
use protobuf::{EnumOrUnknown, Message as _, MessageField};
use serde_json::Value;

use crate::api::client::ApiClient;

/// `queryArtistOverview` persisted-query hashes, newest first. Spotify
/// rotates them when its web player changes; the next is tried when one is
/// no longer known.
const ARTIST_OVERVIEW_HASHES: &[&str] = &[
    "ae0e2958a4ab645b35ca19ac04d0495ae12d9c5d7b7286217674801a9aab281a",
    "7f86ff63e38c24973a2842b672abe44c910c1973978dc8a4a0cb648edef34527",
    "79a4a9d7c3a3781d801e62b62ef11c7ee56fce2626772eb26cd20c69f84b3f49",
];

const TIMEOUT: Duration = Duration::from_secs(30);

/// What the app asks for.
#[derive(Clone, Debug)]
pub enum Request {
    ArtistStats {
        artist_id: String,
    },
    TrackCredits {
        track_id: String,
    },
    /// Appends `uris` (`spotify:local:` URIs) to the end of a playlist.
    AddToPlaylist {
        playlist_id: String,
        name: String,
        uris: Vec<String>,
    },
    /// Removes every row holding one of `uris` (`spotify:local:` URIs,
    /// which the Web API cannot remove).
    RemoveFromPlaylist {
        playlist_id: String,
        uris: Vec<String>,
    },
}

/// What Spotify answered.
#[derive(Clone, Debug)]
pub enum Response {
    ArtistStats {
        artist_id: String,
        result: Result<ArtistStats, String>,
    },
    TrackCredits {
        track_id: String,
        result: Result<Credits, String>,
    },
    AddedToPlaylist {
        playlist_id: String,
        name: String,
        count: usize,
        result: Result<(), String>,
    },
    RemovedFromPlaylist {
        playlist_id: String,
        result: Result<(), String>,
    },
}

impl Request {
    /// The answer when the request cannot be made at all.
    pub fn failed(self, message: &str) -> Response {
        let message = message.to_string();
        match self {
            Request::ArtistStats { artist_id } => Response::ArtistStats {
                artist_id,
                result: Err(message),
            },
            Request::TrackCredits { track_id } => Response::TrackCredits {
                track_id,
                result: Err(message),
            },
            Request::AddToPlaylist {
                playlist_id,
                name,
                uris,
            } => Response::AddedToPlaylist {
                playlist_id,
                name,
                count: uris.len(),
                result: Err(message),
            },
            Request::RemoveFromPlaylist { playlist_id, .. } => Response::RemovedFromPlaylist {
                playlist_id,
                result: Err(message),
            },
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArtistStats {
    pub monthly_listeners: Option<u64>,
    pub followers: Option<u64>,
    pub world_rank: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Credits {
    pub roles: Vec<CreditRole>,
    /// The labels or distributors that supplied the credits.
    pub sources: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CreditRole {
    /// "Performers", "Writers", "Producers", ...
    pub title: String,
    pub people: Vec<CreditPerson>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CreditPerson {
    pub name: String,
    /// A `spotify:artist:` URI when the person has an artist page.
    pub artist_uri: Option<String>,
    /// "main artist", "composer", "producer", ...
    pub subroles: Vec<String>,
}

/// Makes the request with the playback session. `web` is a verified Web API
/// client (for Pathfinder), `http` a client that never retries a write.
pub async fn run(
    session: &Session,
    web: Option<Arc<ApiClient>>,
    http: Option<reqwest::Client>,
    request: Request,
) -> Response {
    let work = async {
        match request.clone() {
            Request::ArtistStats { artist_id } => Response::ArtistStats {
                result: artist_stats(session, web, &artist_id).await,
                artist_id,
            },
            Request::TrackCredits { track_id } => Response::TrackCredits {
                result: track_credits(session, &track_id).await,
                track_id,
            },
            Request::AddToPlaylist {
                playlist_id,
                name,
                uris,
            } => Response::AddedToPlaylist {
                result: add_to_playlist(session, http, &playlist_id, &uris).await,
                count: uris.len(),
                playlist_id,
                name,
            },
            Request::RemoveFromPlaylist { playlist_id, uris } => Response::RemovedFromPlaylist {
                result: remove_from_playlist(session, http, &playlist_id, &uris).await,
                playlist_id,
            },
        }
    };
    match tokio::time::timeout(TIMEOUT, work).await {
        Ok(response) => response,
        Err(_) => request.failed("Spotify took too long to answer. Try again."),
    }
}

async fn artist_stats(
    session: &Session,
    web: Option<Arc<ApiClient>>,
    artist_id: &str,
) -> Result<ArtistStats, String> {
    let web = web.ok_or("Sign in to Spotify first.")?;
    let login = session
        .login5()
        .auth_token()
        .await
        .map_err(|error| error.to_string())?;
    let client_token = session
        .spclient()
        .client_token()
        .await
        .map_err(|error| error.to_string())?;
    let mut last = String::from("Spotify did not return this artist's listeners.");
    for hash in ARTIST_OVERVIEW_HASHES {
        let query = serde_json::json!({
            "operationName": "queryArtistOverview",
            "variables": {
                "uri": format!("spotify:artist:{artist_id}"),
                "locale": "",
                "includePrerelease": false,
            },
            "extensions": {"persistedQuery": {"version": 1, "sha256Hash": hash}},
        });
        match web
            .partner_request(&login.access_token, &client_token, &query)
            .await
        {
            Ok(body) => match parse_artist_stats(&body) {
                Some(stats) => return Ok(stats),
                // An unknown hash answers with an error and no data: try
                // the next.
                None => continue,
            },
            Err(error) => last = error.to_string(),
        }
    }
    Err(last)
}

pub fn parse_artist_stats(body: &Value) -> Option<ArtistStats> {
    let stats = body.pointer("/data/artistUnion/stats")?;
    let number = |key: &str| stats.get(key).and_then(Value::as_u64);
    let found = ArtistStats {
        monthly_listeners: number("monthlyListeners"),
        followers: number("followers"),
        world_rank: number("worldRank").filter(|rank| *rank > 0),
    };
    (found != ArtistStats::default()).then_some(found)
}

async fn track_credits(session: &Session, track_id: &str) -> Result<Credits, String> {
    SpotifyId::from_base62(track_id).map_err(|_| "This is not a Spotify song.".to_string())?;
    let endpoint = format!("/track-credits-view/v0/experimental/{track_id}/credits");
    let body = session
        .spclient()
        .request_as_json(&reqwest::Method::GET, &endpoint, None, None)
        .await
        .map_err(|error| format!("Spotify did not return credits ({error})."))?;
    let json: Value = serde_json::from_slice(&body)
        .map_err(|_| "Spotify sent credits this app cannot read.".to_string())?;
    Ok(parse_credits(&json))
}

/// A string field's trimmed text, when it has any.
fn text(value: &Value) -> Option<&str> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

pub fn parse_credits(json: &Value) -> Credits {
    let roles = json
        .get("roleCredits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|role| {
            let title = role.get("roleTitle").and_then(text)?.to_string();
            let people: Vec<CreditPerson> = role
                .get("artists")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|person| {
                    Some(CreditPerson {
                        name: person.get("name").and_then(text)?.to_string(),
                        artist_uri: person
                            .get("uri")
                            .and_then(text)
                            .filter(|uri| uri.starts_with("spotify:artist:"))
                            .map(str::to_string),
                        subroles: person
                            .get("subroles")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(text)
                            .map(str::to_string)
                            .collect(),
                    })
                })
                .collect();
            (!people.is_empty()).then_some(CreditRole { title, people })
        })
        .collect();
    let sources = json
        .get("sourceNames")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(text)
        .map(str::to_string)
        .collect();
    Credits { roles, sources }
}

/// Appends items the way Spotify's desktop app does: one change to the
/// playlist, based on its current revision, adding at the end. The write is
/// sent once, without retries, so a lost answer cannot add the songs twice.
async fn add_to_playlist(
    session: &Session,
    http: Option<reqwest::Client>,
    playlist_id: &str,
    uris: &[String],
) -> Result<(), String> {
    use librespot_protocol::playlist4_external::{Add, Item, ItemAttributes, Op, op};

    if uris.is_empty() {
        return Ok(());
    }
    let id = SpotifyId::from_base62(playlist_id).map_err(|_| "Unknown playlist.".to_string())?;
    let (revision, _) = playlist_revision(session, &id).await?;
    let username = session.username();
    let now = jiff::Timestamp::now().as_millisecond();
    let items = uris
        .iter()
        .map(|uri| {
            let mut attributes = ItemAttributes::new();
            attributes.added_by = Some(username.clone());
            attributes.timestamp = Some(now);
            let mut item = Item::new();
            item.uri = Some(uri.clone());
            item.attributes = MessageField::some(attributes);
            item
        })
        .collect();
    let mut add = Add::new();
    add.items = items;
    add.add_last = Some(true);
    let mut operation = Op::new();
    operation.kind = Some(EnumOrUnknown::new(op::Kind::ADD));
    operation.add = MessageField::some(add);
    post_changes(session, http, &id, revision, vec![operation]).await
}

/// Removes rows the way Spotify's desktop app does: each row by its
/// position, with its URI so the server can check it is the right one,
/// from the last row up so earlier positions stay put.
async fn remove_from_playlist(
    session: &Session,
    http: Option<reqwest::Client>,
    playlist_id: &str,
    uris: &[String],
) -> Result<(), String> {
    use librespot_protocol::playlist4_external::{Item, Op, Rem, SelectedListContent, op};

    if uris.is_empty() {
        return Ok(());
    }
    let id = SpotifyId::from_base62(playlist_id).map_err(|_| "Unknown playlist.".to_string())?;
    let (revision, length) = playlist_revision(session, &id).await?;
    let wanted: Vec<_> = uris.iter().map(|uri| same_local_key(uri)).collect();
    let mut rows = Vec::new();
    let mut from = 0;
    while from < length {
        let count = (length - from).min(PLAYLIST_READ_PAGE);
        let page = session
            .spclient()
            .get_playlist_range(&id, from, count)
            .await
            .map_err(|error| format!("Could not read the playlist ({error})."))?;
        let page = SelectedListContent::parse_from_bytes(&page)
            .map_err(|_| "Spotify sent a playlist this app cannot read.".to_string())?;
        let items = page
            .contents
            .as_ref()
            .map(|contents| contents.items.clone())
            .unwrap_or_default();
        if items.is_empty() {
            break;
        }
        for (offset, item) in items.iter().enumerate() {
            let uri = item.uri();
            if wanted.contains(&same_local_key(uri)) {
                rows.push((from + offset, uri.to_string()));
            }
        }
        from += items.len();
    }
    if rows.is_empty() {
        return Err("Spotify no longer has that song in this playlist.".into());
    }
    let operations = rows
        .into_iter()
        .rev()
        .map(|(index, uri)| {
            let mut item = Item::new();
            item.uri = Some(uri);
            let mut rem = Rem::new();
            rem.from_index = Some(index as i32);
            rem.length = Some(1);
            rem.items.push(item);
            let mut operation = Op::new();
            operation.kind = Some(EnumOrUnknown::new(op::Kind::REM));
            operation.rem = MessageField::some(rem);
            operation
        })
        .collect();
    post_changes(session, http, &id, revision, operations).await
}

/// A playlist's current revision and length.
async fn playlist_revision(session: &Session, id: &SpotifyId) -> Result<(Vec<u8>, usize), String> {
    use librespot_protocol::playlist4_external::SelectedListContent;

    let header = session
        .spclient()
        .get_playlist_range(id, 0, 0)
        .await
        .map_err(|error| format!("Could not read the playlist ({error})."))?;
    let header = SelectedListContent::parse_from_bytes(&header)
        .map_err(|_| "Spotify sent a playlist this app cannot read.".to_string())?;
    Ok((
        header.revision.clone().unwrap_or_default(),
        usize::try_from(header.length.unwrap_or(0)).unwrap_or(0),
    ))
}

/// Rows read per request while looking for the ones to remove.
const PLAYLIST_READ_PAGE: usize = 100;

/// A local URI compared by what it names, whatever its encoding; other
/// URIs as they are.
fn same_local_key(uri: &str) -> String {
    match parse_local_uri(uri) {
        Some((artist, album, title, seconds)) => format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{seconds}",
            artist.to_lowercase(),
            album.to_lowercase(),
            title.to_lowercase()
        ),
        None => uri.to_string(),
    }
}

/// Sends one change to a playlist, based on `revision`. The write is sent
/// once, without retries, so a lost answer cannot apply it twice.
async fn post_changes(
    session: &Session,
    http: Option<reqwest::Client>,
    id: &SpotifyId,
    revision: Vec<u8>,
    operations: Vec<librespot_protocol::playlist4_external::Op>,
) -> Result<(), String> {
    use librespot_protocol::playlist4_external::{ChangeInfo, Delta, ListChanges};

    let http = http.ok_or("Could not connect to Spotify.")?;
    let mut info = ChangeInfo::new();
    info.user = Some(session.username());
    info.timestamp = Some(jiff::Timestamp::now().as_millisecond());
    let mut delta = Delta::new();
    delta.ops = operations;
    delta.info = MessageField::some(info);
    let mut changes = ListChanges::new();
    changes.base_revision = Some(revision);
    changes.deltas.push(delta);
    changes.want_resulting_revisions = Some(true);
    let body = changes
        .write_to_bytes()
        .map_err(|error| error.to_string())?;

    let endpoint = format!(
        "{}/playlist/v2/playlist/{}/changes",
        session
            .spclient()
            .base_url()
            .await
            .map_err(|error| error.to_string())?,
        id.to_base62()
    );
    let login = session
        .login5()
        .auth_token()
        .await
        .map_err(|error| error.to_string())?;
    let client_token = session
        .spclient()
        .client_token()
        .await
        .map_err(|error| error.to_string())?;
    let response = http
        .post(endpoint)
        .bearer_auth(&login.access_token)
        .header("client-token", client_token)
        .header(reqwest::header::CONTENT_TYPE, "application/x-protobuf")
        .header(reqwest::header::ACCEPT, "application/x-protobuf")
        .body(body)
        .send()
        .await
        .map_err(|error| format!("Could not reach Spotify ({}).", error.without_url()))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!(
            "Spotify did not accept the change (HTTP {}). You may not be allowed to edit this playlist.",
            response.status().as_u16()
        ))
    }
}

/// The `spotify:local:` URI Spotify's apps give a local file: artist,
/// album, title and whole seconds, each form-encoded.
pub fn local_uri(artist: &str, album: &str, title: &str, duration_ms: u32) -> String {
    let encode = |text: &str| urlencoding::encode(text).replace("%20", "+");
    format!(
        "spotify:local:{}:{}:{}:{}",
        encode(artist),
        encode(album),
        encode(title),
        (duration_ms + 500) / 1000
    )
}

/// A local URI's parts, decoded: artist, album, title and seconds.
pub fn parse_local_uri(uri: &str) -> Option<(String, String, String, u32)> {
    let rest = uri.strip_prefix("spotify:local:")?;
    let mut parts = rest.split(':');
    let decode = |text: &str| {
        urlencoding::decode(&text.replace('+', " "))
            .map(|text| text.into_owned())
            .unwrap_or_else(|_| text.replace('+', " "))
    };
    let artist = decode(parts.next()?);
    let album = decode(parts.next()?);
    let title = decode(parts.next()?);
    let seconds = parts.next().and_then(|text| text.parse().ok()).unwrap_or(0);
    Some((artist, album, title, seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_uris_round_trip_like_spotifys() {
        let uri = local_uri("Nina Simone", "Pastel Blues", "Sinnerman: Live", 622_400);
        assert_eq!(
            uri,
            "spotify:local:Nina+Simone:Pastel+Blues:Sinnerman%3A+Live:622"
        );
        assert_eq!(
            parse_local_uri(&uri),
            Some((
                "Nina Simone".into(),
                "Pastel Blues".into(),
                "Sinnerman: Live".into(),
                622
            ))
        );
        assert_eq!(
            parse_local_uri("spotify:local:::Untitled:0"),
            Some((String::new(), String::new(), "Untitled".into(), 0))
        );
    }

    #[test]
    fn local_rows_match_whatever_their_encoding() {
        assert_eq!(
            same_local_key("spotify:local:babyesko:Black+Mass:Nuero:129"),
            same_local_key("spotify:local:Babyesko:Black%20Mass:Nuero:129")
        );
        assert_ne!(
            same_local_key("spotify:local:babyesko:Black+Mass:Nuero:129"),
            same_local_key("spotify:local:babyesko:Black+Mass:Nuero:130")
        );
    }

    #[test]
    fn monthly_listeners_come_from_the_artist_overview() {
        let body = serde_json::json!({"data": {"artistUnion": {"stats": {
            "followers": 1200, "monthlyListeners": 34567890, "worldRank": 0
        }}}});
        let stats = parse_artist_stats(&body).unwrap();
        assert_eq!(stats.monthly_listeners, Some(34_567_890));
        assert_eq!(stats.world_rank, None, "a zero rank means unranked");
        let unknown = serde_json::json!({"errors": [{"message": "PersistedQueryNotFound"}]});
        assert_eq!(parse_artist_stats(&unknown), None);
    }

    #[test]
    fn credits_keep_roles_and_people_in_order() {
        let json = serde_json::json!({
            "roleCredits": [
                {"roleTitle": "Performers", "artists": [
                    {"name": "Olivia Rodrigo", "uri": "spotify:artist:1McMsnEElThX1knmY4oliG", "subroles": ["main artist"]}
                ]},
                {"roleTitle": "Writers", "artists": [
                    {"name": "Daniel Nigro", "uri": "", "subroles": ["composer", "lyricist"]}
                ]},
                {"roleTitle": "Producers", "artists": []}
            ],
            "sourceNames": ["Olivia Rodrigo PS"]
        });
        let credits = parse_credits(&json);
        assert_eq!(credits.roles.len(), 2, "an empty role is left out");
        assert_eq!(
            credits.roles[0].people[0].artist_uri.as_deref(),
            Some("spotify:artist:1McMsnEElThX1knmY4oliG")
        );
        assert_eq!(credits.roles[1].people[0].artist_uri, None);
        assert_eq!(
            credits.roles[1].people[0].subroles,
            ["composer", "lyricist"]
        );
        assert_eq!(credits.sources, ["Olivia Rodrigo PS"]);
    }
}

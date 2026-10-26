//! SoundCloud without an account: search, links, and the audio of a track,
//! read the way SoundCloud's own website reads them.
//!
//! The website calls api-v2.soundcloud.com with a public client id that is
//! written into its scripts; this does the same, finding the id when it
//! starts and again when SoundCloud changes it. A track's audio is
//! downloaded whole into the cache folder and played from there by the Local
//! Files player. All of this is unofficial: SoundCloud can change any of it.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;

const SITE: &str = "https://soundcloud.com";
const API: &str = "https://api-v2.soundcloud.com";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36";
/// Tracks kept in the cache folder; the oldest go first.
const CACHE_TRACKS: usize = 60;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Track {
    pub id: u64,
    pub title: String,
    pub artist: String,
    pub duration_ms: u32,
    /// A large cover, when the track or its uploader has one.
    pub artwork: Option<String>,
    pub permalink: String,
    pub plays: Option<u64>,
    /// Only a preview plays without SoundCloud Go+.
    pub preview: bool,
    transcodings: Vec<Transcoding>,
    authorization: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Transcoding {
    url: String,
    protocol: String,
    mime: String,
}

impl Track {
    /// Whether any of its audio is something this app can play.
    pub fn playable(&self) -> bool {
        best_transcoding(&self.transcodings).is_some()
    }
}

pub struct Client {
    http: reqwest::blocking::Client,
    client_id: Mutex<Option<String>>,
    cache: PathBuf,
}

impl Client {
    pub fn new(cache: PathBuf) -> Result<Self, String> {
        let http = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            http,
            client_id: Mutex::new(None),
            cache,
        })
    }

    /// The website's client id, found once and kept until it stops working.
    fn client_id(&self, fresh: bool) -> Result<String, String> {
        let mut held = self.client_id.lock().unwrap_or_else(|p| p.into_inner());
        if !fresh && let Some(id) = held.as_ref() {
            return Ok(id.clone());
        }
        let page = self.get_text(SITE)?;
        // The id is in one of the site's scripts, usually one of the last.
        for script in script_urls(&page).into_iter().rev() {
            if let Ok(text) = self.get_text(&script)
                && let Some(id) = find_client_id(&text)
            {
                *held = Some(id.clone());
                return Ok(id);
            }
        }
        Err("Couldn't connect to SoundCloud (it may have changed its site).".into())
    }

    fn get_text(&self, url: &str) -> Result<String, String> {
        let response = self
            .http
            .get(url)
            .send()
            .map_err(|error| format!("Couldn't reach SoundCloud ({}).", error.without_url()))?;
        if !response.status().is_success() {
            return Err(format!(
                "SoundCloud answered {}.",
                response.status().as_u16()
            ));
        }
        response.text().map_err(|error| error.to_string())
    }

    /// An api-v2 read with the client id, finding a new id once if the old
    /// one has stopped working.
    fn api(&self, path_and_query: &str) -> Result<Value, String> {
        for fresh in [false, true] {
            let id = self.client_id(fresh)?;
            let separator = if path_and_query.contains('?') {
                '&'
            } else {
                '?'
            };
            let url = if path_and_query.starts_with("https://") {
                format!("{path_and_query}{separator}client_id={id}")
            } else {
                format!("{API}{path_and_query}{separator}client_id={id}")
            };
            let response = self
                .http
                .get(&url)
                .header("Origin", SITE)
                .header("Referer", format!("{SITE}/"))
                .send()
                .map_err(|error| format!("Couldn't reach SoundCloud ({}).", error.without_url()))?;
            let status = response.status().as_u16();
            if (status == 401 || status == 403) && !fresh {
                continue;
            }
            if status == 404 {
                return Err("SoundCloud doesn't have that.".into());
            }
            if !(200..300).contains(&status) {
                return Err(format!("SoundCloud answered {status}."));
            }
            return response
                .json::<Value>()
                .map_err(|_| "SoundCloud sent something this app can't read.".to_string());
        }
        Err("SoundCloud refused the request.".into())
    }

    pub fn search(&self, query: &str) -> Result<Vec<Track>, String> {
        let body = self.api(&format!(
            "/search/tracks?q={}&limit=50&linked_partitioning=1",
            urlencoding::encode(query)
        ))?;
        Ok(body["collection"]
            .as_array()
            .map(|list| list.iter().filter_map(parse_track).collect())
            .unwrap_or_default())
    }

    /// The tracks behind a soundcloud.com link: one track, or a playlist's.
    pub fn resolve(&self, link: &str) -> Result<Vec<Track>, String> {
        let body = self.api(&format!(
            "/resolve?url={}",
            urlencoding::encode(link.trim())
        ))?;
        match body["kind"].as_str() {
            Some("track") => Ok(parse_track(&body).into_iter().collect()),
            Some("playlist") | Some("system-playlist") => {
                let listed = body["tracks"].as_array().cloned().unwrap_or_default();
                let mut tracks: Vec<Option<Track>> = listed.iter().map(parse_track).collect();
                // A long playlist sends only the ids of its later tracks.
                let missing: Vec<u64> = listed
                    .iter()
                    .zip(&tracks)
                    .filter(|(_, track)| track.is_none())
                    .filter_map(|(item, _)| item["id"].as_u64())
                    .collect();
                for ids in missing.chunks(50) {
                    let joined = ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
                    let full = self.api(&format!("/tracks?ids={joined}"))?;
                    for item in full.as_array().into_iter().flatten() {
                        if let Some(track) = parse_track(item)
                            && let Some(slot) = listed
                                .iter()
                                .position(|entry| entry["id"].as_u64() == Some(track.id))
                        {
                            tracks[slot] = Some(track);
                        }
                    }
                }
                Ok(tracks.into_iter().flatten().collect())
            }
            Some("user") => Err("That's a profile. Paste a link to a track or playlist.".into()),
            _ => Err("That link isn't a SoundCloud track or playlist.".into()),
        }
    }

    /// The track's audio in the cache folder, downloading it if it is not
    /// there yet, with its cover beside it for the player to show.
    pub fn download(&self, track: &Track) -> Result<PathBuf, String> {
        let folder = self.cache.join(track.id.to_string());
        for name in ["audio.mp3", "audio.m4a"] {
            let path = folder.join(name);
            if path.is_file() {
                return Ok(path);
            }
        }
        let transcoding = best_transcoding(&track.transcodings)
            .ok_or("SoundCloud doesn't offer this track in a form the app can play.")?;
        let mut query = transcoding.url.clone();
        if let Some(authorization) = &track.authorization {
            let separator = if query.contains('?') { '&' } else { '?' };
            query = format!("{query}{separator}track_authorization={authorization}");
        }
        let location = self.api(&query)?["url"]
            .as_str()
            .ok_or("SoundCloud didn't say where the audio is.")?
            .to_string();
        let audio = if transcoding.protocol == "progressive" {
            self.get_bytes(&location)?
        } else {
            let playlist = self.get_text(&location)?;
            let (init, segments) = hls_parts(&playlist, &location);
            if segments.is_empty() {
                return Err("SoundCloud sent an empty stream.".into());
            }
            let mut audio = Vec::new();
            if let Some(init) = init {
                audio.extend(self.get_bytes(&init)?);
            }
            for segment in segments {
                audio.extend(self.get_bytes(&segment)?);
            }
            audio
        };
        std::fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
        let name = if transcoding.mime.contains("mpeg") {
            "audio.mp3"
        } else {
            "audio.m4a"
        };
        if let Some(artwork) = &track.artwork
            && let Ok(cover) = self.get_bytes(artwork)
        {
            let _ = std::fs::write(folder.join("cover.jpg"), cover);
        }
        let path = folder.join(name);
        let partial = folder.join(format!("{name}.part"));
        std::fs::write(&partial, audio).map_err(|error| error.to_string())?;
        std::fs::rename(&partial, &path).map_err(|error| error.to_string())?;
        trim_cache(&self.cache);
        Ok(path)
    }

    fn get_bytes(&self, url: &str) -> Result<Vec<u8>, String> {
        let response = self.http.get(url).send().map_err(|error| {
            format!(
                "Couldn't download from SoundCloud ({}).",
                error.without_url()
            )
        })?;
        if !response.status().is_success() {
            return Err(format!(
                "SoundCloud answered {}.",
                response.status().as_u16()
            ));
        }
        response
            .bytes()
            .map(|bytes| bytes.to_vec())
            .map_err(|error| error.to_string())
    }
}

/// The scripts a soundcloud.com page loads from its asset host.
fn script_urls(page: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut rest = page;
    while let Some(start) = rest.find("src=\"") {
        rest = &rest[start + 5..];
        let Some(end) = rest.find('"') else {
            break;
        };
        let url = &rest[..end];
        if url.starts_with("https://") && url.contains("sndcdn.com") && url.ends_with(".js") {
            urls.push(url.to_string());
        }
        rest = &rest[end..];
    }
    urls
}

/// `client_id:"…"` (32 letters and digits) in a script.
fn find_client_id(script: &str) -> Option<String> {
    let mut rest = script;
    while let Some(start) = rest.find("client_id") {
        rest = &rest[start + "client_id".len()..];
        let trimmed = rest.trim_start_matches(|c: char| c == ' ' || c == ':' || c == '=');
        let Some(quoted) = trimmed.strip_prefix('"') else {
            continue;
        };
        let id: String = quoted
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect();
        if id.len() == 32 && quoted[id.len()..].starts_with('"') {
            return Some(id);
        }
    }
    None
}

fn parse_track(item: &Value) -> Option<Track> {
    // A playlist's later tracks come as ids alone, without a title.
    let title = item["title"].as_str()?.to_string();
    let id = item["id"].as_u64()?;
    let transcodings = item["media"]["transcodings"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|entry| {
                    Some(Transcoding {
                        url: entry["url"].as_str()?.to_string(),
                        protocol: entry["format"]["protocol"].as_str()?.to_string(),
                        mime: entry["format"]["mime_type"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let artwork = item["artwork_url"]
        .as_str()
        .or_else(|| item["user"]["avatar_url"].as_str())
        .map(|url| url.replace("-large.", "-t500x500."));
    Some(Track {
        id,
        title,
        artist: item["user"]["username"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        duration_ms: item["full_duration"]
            .as_u64()
            .or_else(|| item["duration"].as_u64())
            .unwrap_or(0)
            .min(u64::from(u32::MAX)) as u32,
        artwork,
        permalink: item["permalink_url"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        plays: item["playback_count"].as_u64(),
        preview: item["policy"].as_str() == Some("SNIP"),
        transcodings,
        authorization: item["track_authorization"].as_str().map(str::to_string),
    })
}

/// The audio to fetch: a whole MP3 file first, then an MP3 or AAC stream in
/// pieces. Encrypted streams and Opus are left out: they don't play here.
fn best_transcoding(list: &[Transcoding]) -> Option<&Transcoding> {
    let rank = |transcoding: &Transcoding| -> Option<u8> {
        let mpeg = transcoding.mime.contains("mpeg");
        let aac = transcoding.mime.contains("mp4") || transcoding.mime.contains("aac");
        match (transcoding.protocol.as_str(), mpeg, aac) {
            ("progressive", true, _) => Some(0),
            ("hls", true, _) => Some(1),
            ("progressive", _, true) => Some(2),
            ("hls", _, true) => Some(3),
            _ => None,
        }
    };
    list.iter()
        .filter_map(|transcoding| rank(transcoding).map(|rank| (rank, transcoding)))
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, transcoding)| transcoding)
}

/// An HLS playlist's initialisation section (for MP4 pieces) and pieces,
/// as full URLs.
fn hls_parts(playlist: &str, base: &str) -> (Option<String>, Vec<String>) {
    let absolute = |uri: &str| -> String {
        if uri.starts_with("http://") || uri.starts_with("https://") {
            uri.to_string()
        } else if let Some(path) = uri.strip_prefix('/') {
            let origin = base.splitn(4, '/').take(3).collect::<Vec<_>>().join("/");
            format!("{origin}/{path}")
        } else {
            let folder = base.split('?').next().unwrap_or(base);
            let folder = &folder[..folder.rfind('/').map_or(0, |i| i + 1)];
            format!("{folder}{uri}")
        }
    };
    let mut init = None;
    let mut segments = Vec::new();
    for line in playlist.lines().map(str::trim) {
        if let Some(attributes) = line.strip_prefix("#EXT-X-MAP:") {
            if let Some(start) = attributes.find("URI=\"") {
                let rest = &attributes[start + 5..];
                if let Some(end) = rest.find('"') {
                    init = Some(absolute(&rest[..end]));
                }
            }
        } else if !line.is_empty() && !line.starts_with('#') {
            segments.push(absolute(line));
        }
    }
    (init, segments)
}

/// Keeps the cache folder to the newest tracks.
fn trim_cache(cache: &Path) {
    let Ok(entries) = std::fs::read_dir(cache) else {
        return;
    };
    let mut folders: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect();
    if folders.len() <= CACHE_TRACKS {
        return;
    }
    folders.sort();
    for (_, folder) in &folders[..folders.len() - CACHE_TRACKS] {
        let _ = std::fs::remove_dir_all(folder);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_client_id_is_read_from_the_sites_scripts() {
        let page = r#"<script crossorigin src="https://a-v2.sndcdn.com/assets/0-abc.js"></script>
            <script src="https://a-v2.sndcdn.com/assets/49-def.js"></script><img src="https://x.png">"#;
        assert_eq!(
            script_urls(page),
            [
                "https://a-v2.sndcdn.com/assets/0-abc.js",
                "https://a-v2.sndcdn.com/assets/49-def.js"
            ]
        );
        let script = r#"...{client_id:"a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6",env:"production"}..."#;
        assert_eq!(
            find_client_id(script).as_deref(),
            Some("a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6")
        );
        assert_eq!(find_client_id(r#"client_id:"short""#), None);
    }

    #[test]
    fn tracks_and_their_best_audio_are_read() {
        let item = serde_json::json!({
            "kind": "track", "id": 123, "title": "Song", "duration": 30000, "full_duration": 180000,
            "artwork_url": "https://i1.sndcdn.com/artworks-x-large.jpg",
            "permalink_url": "https://soundcloud.com/a/song", "playback_count": 42, "policy": "ALLOW",
            "track_authorization": "auth", "user": {"username": "Artist"},
            "media": {"transcodings": [
                {"url": "https://api-v2.soundcloud.com/media/1/opus", "format": {"protocol": "hls", "mime_type": "audio/ogg; codecs=\"opus\""}},
                {"url": "https://api-v2.soundcloud.com/media/1/aac", "format": {"protocol": "hls", "mime_type": "audio/mp4; codecs=\"mp4a.40.2\""}},
                {"url": "https://api-v2.soundcloud.com/media/1/enc", "format": {"protocol": "ctr-encrypted-hls", "mime_type": "audio/mpeg"}},
                {"url": "https://api-v2.soundcloud.com/media/1/mp3", "format": {"protocol": "hls", "mime_type": "audio/mpeg"}}
            ]}
        });
        let track = parse_track(&item).unwrap();
        assert_eq!(track.artist, "Artist");
        assert_eq!(track.duration_ms, 180000);
        assert_eq!(
            track.artwork.as_deref(),
            Some("https://i1.sndcdn.com/artworks-x-t500x500.jpg")
        );
        assert!(!track.preview && track.playable());
        assert_eq!(
            best_transcoding(&track.transcodings).unwrap().url,
            "https://api-v2.soundcloud.com/media/1/mp3"
        );
        assert!(
            parse_track(&serde_json::json!({"id": 5})).is_none(),
            "ids alone are filled in later"
        );
    }

    #[test]
    fn hls_pieces_become_full_urls() {
        let playlist = "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:10,\nseg1.m4s?x=1\n#EXTINF:10,\nhttps://cdn.example/seg2.m4s\n#EXT-X-ENDLIST\n";
        let (init, segments) = hls_parts(
            playlist,
            "https://cdn.example/media/abc/playlist.m3u8?sig=1",
        );
        assert_eq!(
            init.as_deref(),
            Some("https://cdn.example/media/abc/init.mp4")
        );
        assert_eq!(
            segments,
            [
                "https://cdn.example/media/abc/seg1.m4s?x=1",
                "https://cdn.example/seg2.m4s"
            ]
        );
    }
}

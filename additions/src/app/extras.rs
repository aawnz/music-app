//! Monthly listeners, song credits and adding local files to playlists, on
//! the app side: what has been asked for, what came back, and what the
//! credits window shows. The requests themselves are in `crate::extras`.

use super::*;
use crate::extras::{ArtistStats, Credits, Request, Response};

/// An artist's listener count, once asked for.
enum ArtistInfo {
    Pending,
    Ready(ArtistStats),
    /// Asked again a minute later: playback may still have been starting.
    Failed(Instant),
}

const RETRY_AFTER: Duration = Duration::from_secs(60);

/// What the credits window shows.
pub enum CreditsState {
    Loading,
    Ready(Credits),
    Failed(String),
}

pub struct CreditsView {
    pub track_id: String,
    pub title: String,
    pub artists: String,
    pub state: CreditsState,
}

#[derive(Default)]
pub struct Extras {
    artists: HashMap<String, ArtistInfo>,
    /// The open credits window.
    pub credits: Option<CreditsView>,
    /// Asked for during the frame, sent after it.
    requests: Vec<Request>,
}

impl Extras {
    /// The artist's monthly listeners, when Spotify has said. The first
    /// call for an artist asks.
    pub fn monthly_listeners(&mut self, artist_id: &str) -> Option<u64> {
        if artist_id.is_empty() {
            return None;
        }
        match self.artists.get(artist_id) {
            Some(ArtistInfo::Ready(stats)) => stats.monthly_listeners,
            Some(ArtistInfo::Pending) => None,
            Some(ArtistInfo::Failed(at)) if at.elapsed() < RETRY_AFTER => None,
            _ => {
                self.artists
                    .insert(artist_id.to_string(), ArtistInfo::Pending);
                self.requests.push(Request::ArtistStats {
                    artist_id: artist_id.to_string(),
                });
                None
            }
        }
    }

    /// Opens the credits window for a song and asks Spotify for them.
    pub fn open_credits(&mut self, track_id: &str, title: &str, artists: &str) {
        self.credits = Some(CreditsView {
            track_id: track_id.to_string(),
            title: title.to_string(),
            artists: artists.to_string(),
            state: CreditsState::Loading,
        });
        self.requests.push(Request::TrackCredits {
            track_id: track_id.to_string(),
        });
    }

    pub fn close_credits(&mut self) {
        self.credits = None;
    }
}

impl App {
    /// Sends what was asked for during the frame.
    pub(crate) fn extras_frame(&mut self) {
        for request in std::mem::take(&mut self.extras.requests) {
            self.backend.send(Command::Extra(request));
        }
    }

    /// Appends local files to one of the account's playlists.
    pub(crate) fn add_local_to_playlist(
        &mut self,
        playlist_id: String,
        name: String,
        uris: Vec<String>,
    ) {
        if uris.is_empty() {
            return;
        }
        let count = uris.len();
        self.toast(if count == 1 {
            format!("Adding 1 song to {name}…")
        } else {
            format!("Adding {count} songs to {name}…")
        });
        self.extras.requests.push(Request::AddToPlaylist {
            playlist_id,
            name,
            uris,
        });
    }

    /// Removes local files from a playlist. The Web API refuses their URIs,
    /// so they go through the same playlist service Spotify's apps use; the
    /// rows leave the page at once.
    pub(crate) fn remove_local_from_playlist(&mut self, playlist_id: String, uris: Vec<String>) {
        if uris.is_empty() {
            return;
        }
        if let Some(page) = self.playlist_pages.get_mut(&playlist_id) {
            let before = page.items.items.len();
            page.items.retain(|item| {
                item.playable()
                    .is_none_or(|playable| !uris.iter().any(|uri| uri == playable.uri()))
            });
            let removed = (before - page.items.items.len()) as u32;
            page.items.total = page.items.total.map(|total| total.saturating_sub(removed));
            page.items.next_offset = page
                .items
                .next_offset
                .map(|offset| offset.saturating_sub(removed));
        }
        self.extras
            .requests
            .push(Request::RemoveFromPlaylist { playlist_id, uris });
    }

    pub(crate) fn apply_extra(&mut self, response: Response) {
        match response {
            Response::ArtistStats { artist_id, result } => {
                let info = match result {
                    Ok(stats) => ArtistInfo::Ready(stats),
                    Err(error) => {
                        log::info!("monthly listeners unavailable for {artist_id}: {error}");
                        ArtistInfo::Failed(Instant::now())
                    }
                };
                self.extras.artists.insert(artist_id, info);
            }
            Response::TrackCredits { track_id, result } => {
                if let Some(view) = self.extras.credits.as_mut()
                    && view.track_id == track_id
                {
                    view.state = match result {
                        Ok(credits) => CreditsState::Ready(credits),
                        Err(error) => CreditsState::Failed(error),
                    };
                }
            }
            Response::AddedToPlaylist {
                playlist_id,
                name,
                count,
                result,
            } => match result {
                Ok(()) => {
                    self.toast(if count == 1 {
                        format!("Added to {name}")
                    } else {
                        format!("Added {count} songs to {name}")
                    });
                    self.actions
                        .push(Action::Reload(Page::Playlist(playlist_id)));
                }
                Err(error) => self.toast_error(format!("Couldn't add to {name}: {error}")),
            },
            Response::RemovedFromPlaylist {
                playlist_id,
                result,
            } => {
                match result {
                    Ok(()) => self.toast("Removed from playlist"),
                    Err(error) => self.toast_error(format!("Couldn't remove it: {error}")),
                }
                // Either way, show the playlist as Spotify has it now.
                self.actions
                    .push(Action::Reload(Page::Playlist(playlist_id)));
            }
        }
    }
}

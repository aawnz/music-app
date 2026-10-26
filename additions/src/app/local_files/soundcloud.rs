//! The SoundCloud tab: searching, links, and a queue of SoundCloud tracks
//! played one at a time by the Local Files player, from audio downloaded to
//! the cache folder. See `crate::soundcloud` for how SoundCloud is read.

use super::*;
use crate::soundcloud::{Client, Track};
use std::sync::mpsc;

/// Previous restarts a track played longer than this, as Spotify does.
const RESTART_AFTER_MS: u32 = 3_000;

enum Reply {
    Results {
        generation: u64,
        result: Result<Vec<Track>, String>,
    },
    Downloaded {
        id: u64,
        result: Result<std::path::PathBuf, String>,
    },
}

/// What the SoundCloud tab shows, and what it plays.
#[derive(Default)]
pub struct ScState {
    client: Option<Arc<Client>>,
    replies: Option<(mpsc::Sender<Reply>, mpsc::Receiver<Reply>)>,
    pub query: String,
    pub results: Vec<Track>,
    pub searching: bool,
    pub error: Option<String>,
    /// What the shown results are for.
    pub shown_for: Option<String>,
    generation: u64,
    /// The tracks playing, in play order, and the one playing.
    pub queue: Vec<Track>,
    /// The queue as it was chosen, for shuffle to go back to.
    original: Vec<Track>,
    pub index: usize,
    pub shuffle: bool,
    /// SoundCloud has the bar: the local player plays its tracks.
    pub active: bool,
    /// The track being fetched to play now.
    pub loading: Option<u64>,
    /// Tracks downloaded, by id.
    ready: HashMap<u64, std::path::PathBuf>,
    /// Downloads under way, so each starts once.
    fetching: HashSet<u64>,
    /// Pushed by the views, applied after the frame.
    pub actions: Vec<ScAction>,
}

/// What the SoundCloud tab and the bar ask for.
#[derive(Clone, Debug)]
pub enum ScAction {
    Search,
    /// Plays `tracks[index]`, then the rest.
    Play {
        tracks: Vec<Track>,
        index: usize,
    },
    AddToQueue(Track),
    OpenInBrowser(String),
}

impl ScState {
    /// The track playing, when SoundCloud has the bar.
    pub fn current(&self) -> Option<&Track> {
        self.active.then(|| self.queue.get(self.index)).flatten()
    }
}

impl App {
    fn sc_client(&mut self) -> Result<Arc<Client>, String> {
        if let Some(client) = &self.local_library.soundcloud.client {
            return Ok(Arc::clone(client));
        }
        let client = Arc::new(Client::new(self.dirs.cache.join("soundcloud"))?);
        self.local_library.soundcloud.client = Some(Arc::clone(&client));
        Ok(client)
    }

    fn sc_sender(&mut self) -> mpsc::Sender<Reply> {
        let replies = self
            .local_library
            .soundcloud
            .replies
            .get_or_insert_with(mpsc::channel);
        replies.0.clone()
    }

    pub(crate) fn apply_sc_action(&mut self, action: ScAction, ctx: &egui::Context) {
        match action {
            ScAction::Search => self.sc_search(ctx),
            ScAction::Play { tracks, index } => self.sc_play(tracks, index, ctx),
            ScAction::AddToQueue(track) => {
                let sc = &mut self.local_library.soundcloud;
                if sc.active {
                    sc.original.push(track.clone());
                    sc.queue.push(track.clone());
                    self.toast(format!("Added {} to the queue", track.title));
                } else {
                    self.sc_play(vec![track], 0, ctx);
                }
            }
            ScAction::OpenInBrowser(url) => {
                if let Err(error) = open::that_detached(&url) {
                    self.toast_error(format!("Couldn't open the link: {error}"));
                }
            }
        }
    }

    fn sc_search(&mut self, ctx: &egui::Context) {
        let query = self.local_library.soundcloud.query.trim().to_string();
        if query.is_empty() {
            return;
        }
        let client = match self.sc_client() {
            Ok(client) => client,
            Err(error) => {
                self.local_library.soundcloud.error = Some(error);
                return;
            }
        };
        let sender = self.sc_sender();
        let sc = &mut self.local_library.soundcloud;
        sc.generation += 1;
        sc.searching = true;
        sc.error = None;
        sc.shown_for = Some(query.clone());
        let generation = sc.generation;
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let link = query.contains("soundcloud.com/") || query.contains("on.soundcloud.com");
            let result = if link {
                let link = if query.starts_with("http") {
                    query
                } else {
                    format!("https://{query}")
                };
                client.resolve(&link)
            } else {
                client.search(&query)
            };
            let _ = sender.send(Reply::Results { generation, result });
            ctx.request_repaint();
        });
    }

    /// Downloads a track in the background, once.
    fn sc_fetch(&mut self, track: &Track, ctx: &egui::Context) {
        let sc = &self.local_library.soundcloud;
        if sc.ready.contains_key(&track.id) || sc.fetching.contains(&track.id) {
            return;
        }
        let Ok(client) = self.sc_client() else {
            return;
        };
        let sender = self.sc_sender();
        self.local_library.soundcloud.fetching.insert(track.id);
        let track = track.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = client.download(&track);
            if let Err(error) = &result {
                log::info!("soundcloud: {} not downloaded: {error}", track.id);
            }
            let _ = sender.send(Reply::Downloaded {
                id: track.id,
                result,
            });
            ctx.request_repaint();
        });
    }

    fn sc_play(&mut self, tracks: Vec<Track>, index: usize, ctx: &egui::Context) {
        if tracks.is_empty() {
            return;
        }
        let index = index.min(tracks.len() - 1);
        let sc = &mut self.local_library.soundcloud;
        sc.original = tracks.clone();
        if sc.shuffle {
            let chosen = tracks[index].clone();
            let mut rest: Vec<Track> = tracks
                .into_iter()
                .enumerate()
                .filter(|(at, _)| *at != index)
                .map(|(_, track)| track)
                .collect();
            use rand::seq::SliceRandom;
            rest.shuffle(&mut rand::rng());
            sc.queue = std::iter::once(chosen).chain(rest).collect();
            sc.index = 0;
        } else {
            sc.queue = tracks;
            sc.index = index;
        }
        let at = sc.index;
        self.sc_start(at, ctx);
    }

    /// Plays the queue's `index`: at once when it is downloaded, else when
    /// its download arrives.
    fn sc_start(&mut self, index: usize, ctx: &egui::Context) {
        let Some(track) = self.local_library.soundcloud.queue.get(index).cloned() else {
            // The end of the queue: around again with repeat, else stop.
            if self.local_library.repeat == Repeat::All
                && !self.local_library.soundcloud.queue.is_empty()
            {
                self.sc_start(0, ctx);
            } else {
                self.local_library.soundcloud.active = false;
            }
            return;
        };
        let sc = &mut self.local_library.soundcloud;
        sc.index = index;
        sc.active = true;
        if let Some(path) = sc.ready.get(&track.id).cloned() {
            sc.loading = None;
            self.sc_play_file(&track, path, ctx);
        } else {
            sc.loading = Some(track.id);
            if self.spotify_playing() {
                self.toggle_play();
            }
            self.sc_fetch(&track, ctx);
        }
    }

    fn sc_play_file(&mut self, track: &Track, path: std::path::PathBuf, ctx: &egui::Context) {
        let song = LocalTrack {
            path: path.to_string_lossy().into_owned(),
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: "SoundCloud".into(),
            duration_ms: track.duration_ms,
            ..LocalTrack::default()
        };
        self.play_local_rows(vec![song], 0, ctx);
        // One track at a time, so the queue decides what follows: the
        // player itself only repeats a track.
        let repeat = self.local_library.repeat;
        if repeat == Repeat::All {
            self.local_send(ctx, LocalCommand::SetRepeat(Repeat::Off));
        }
        self.local_library.soundcloud.active = true;
        // The next track downloads while this one plays.
        let next = self.local_library.soundcloud.index + 1;
        if let Some(next) = self.local_library.soundcloud.queue.get(next).cloned() {
            self.sc_fetch(&next, ctx);
        }
    }

    fn sc_next(&mut self, ctx: &egui::Context) {
        let next = self.local_library.soundcloud.index + 1;
        self.sc_start(next, ctx);
    }

    fn sc_previous(&mut self, ctx: &egui::Context) {
        let index = self.local_library.soundcloud.index;
        if self.local_library.position_ms > RESTART_AFTER_MS || index == 0 {
            self.local_library.position_ms = 0;
            self.local_send(ctx, LocalCommand::Seek(0));
        } else {
            self.sc_start(index - 1, ctx);
        }
    }

    fn sc_set_shuffle(&mut self, shuffle: bool) {
        let sc = &mut self.local_library.soundcloud;
        sc.shuffle = shuffle;
        let Some(current) = sc.queue.get(sc.index).cloned() else {
            return;
        };
        if shuffle {
            let mut rest: Vec<Track> = sc
                .original
                .iter()
                .filter(|track| track.id != current.id)
                .cloned()
                .collect();
            use rand::seq::SliceRandom;
            rest.shuffle(&mut rand::rng());
            sc.queue = std::iter::once(current).chain(rest).collect();
            sc.index = 0;
        } else {
            sc.queue = sc.original.clone();
            sc.index = sc
                .queue
                .iter()
                .position(|track| track.id == current.id)
                .unwrap_or(0);
        }
    }

    /// The bar's buttons while a SoundCloud track plays.
    pub(crate) fn sc_take_local(&mut self, action: &LocalAction, ctx: &egui::Context) -> bool {
        if !self.local_library.soundcloud.active {
            return false;
        }
        match action {
            LocalAction::Next => self.sc_next(ctx),
            LocalAction::Previous => self.sc_previous(ctx),
            LocalAction::ToggleShuffle => {
                let shuffle = !self.local_library.soundcloud.shuffle;
                self.sc_set_shuffle(shuffle);
            }
            LocalAction::CycleRepeat => {
                self.local_library.repeat = self.local_library.repeat.next();
                let repeat = self.local_library.repeat;
                // The queue repeats itself; the player only a track.
                let player = if repeat == Repeat::One {
                    Repeat::One
                } else {
                    Repeat::Off
                };
                self.local_send(ctx, LocalCommand::SetRepeat(player));
                self.save_local_files();
            }
            LocalAction::Play { .. }
            | LocalAction::ShuffleAll(_)
            | LocalAction::PlayRow { .. }
            | LocalAction::PlayUris { .. } => {
                self.sc_leave(ctx);
                return false;
            }
            _ => return false,
        }
        true
    }

    /// SoundCloud gives up the bar.
    fn sc_leave(&mut self, ctx: &egui::Context) {
        let sc = &mut self.local_library.soundcloud;
        if !sc.active {
            return;
        }
        sc.active = false;
        sc.loading = None;
        let repeat = self.local_library.repeat;
        self.local_send(ctx, LocalCommand::SetRepeat(repeat));
    }

    /// Runs once a frame: reads answers, and moves on when a track ends.
    pub(crate) fn sc_frame(&mut self, ctx: &egui::Context) {
        for action in std::mem::take(&mut self.local_library.soundcloud.actions) {
            self.apply_sc_action(action, ctx);
        }
        let mut replies = Vec::new();
        if let Some((_, receiver)) = &self.local_library.soundcloud.replies {
            while let Ok(reply) = receiver.try_recv() {
                replies.push(reply);
            }
        }
        for reply in replies {
            match reply {
                Reply::Results { generation, result } => {
                    let sc = &mut self.local_library.soundcloud;
                    if generation != sc.generation {
                        continue;
                    }
                    sc.searching = false;
                    match result {
                        Ok(tracks) => {
                            sc.error = tracks
                                .is_empty()
                                .then(|| "Nothing on SoundCloud matches that.".to_string());
                            sc.results = tracks;
                        }
                        Err(error) => sc.error = Some(error),
                    }
                }
                Reply::Downloaded { id, result } => {
                    let sc = &mut self.local_library.soundcloud;
                    sc.fetching.remove(&id);
                    match result {
                        Ok(path) => {
                            sc.ready.insert(id, path.clone());
                            if sc.loading == Some(id) {
                                sc.loading = None;
                                if let Some(track) = sc.queue.get(sc.index).cloned()
                                    && track.id == id
                                {
                                    self.sc_play_file(&track, path, ctx);
                                }
                            }
                        }
                        Err(error) => {
                            if sc.loading == Some(id) {
                                sc.loading = None;
                                let title = sc
                                    .queue
                                    .get(sc.index)
                                    .map(|track| track.title.clone())
                                    .unwrap_or_default();
                                self.toast_error(format!("Couldn't play {title}: {error}"));
                                // On to the next one, as Spotify skips what won't play.
                                self.sc_next(ctx);
                            }
                        }
                    }
                }
            }
        }
        let sc = &self.local_library.soundcloud;
        if !sc.active || sc.loading.is_some() {
            return;
        }
        if !self.local_library.active {
            // Something else took the bar.
            self.sc_leave(ctx);
        } else if self.local_library.expected.is_none() && self.local_library.status.finished {
            self.sc_next(ctx);
        }
    }
}

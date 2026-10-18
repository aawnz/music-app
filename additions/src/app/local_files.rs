//! Local files on the app side: the library on screen, the queue that was
//! asked for, and which player the bar and the playback keys belong to.
//!
//! The views never touch the player. They push [`LocalAction`]s, which
//! [`App::local_files_frame`] applies after the frame's other actions,
//! because starting a song here also pauses Spotify.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use super::*;
use crate::local_files::{
    Command as LocalCommand, LibraryFile, LocalTrack, Player, Repeat, ScanProgress, Status,
    load_library, save_library, start_scan,
};

/// How long the bar shows what was just asked for before the player has
/// answered.
const OPTIMISTIC_HOLD: Duration = Duration::from_millis(1500);

/// Spotify starting to play within this long of a local song starting is
/// the pause we sent catching up, not the listener switching back.
const SWITCH_GRACE: Duration = Duration::from_secs(5);
/// How long to wait for more local songs Spotify skips in a row.
const HANDOFF_GATHER: Duration = Duration::from_millis(800);

/// Columns the song list sorts by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LocalSort {
    /// Artist, album, then track number: the order a scan gives.
    #[default]
    Artist,
    Title,
    Album,
    Duration,
}

/// What the Local Files page and bar ask for.
#[derive(Clone, Debug)]
pub enum LocalAction {
    AddFolder,
    AddFolderPath(String),
    RemoveFolder(String),
    Rescan,
    /// Plays `rows[index]`, then the rest of `rows`.
    Play {
        rows: Vec<LocalTrack>,
        index: usize,
    },
    /// Plays `rows` shuffled.
    ShuffleAll(Vec<LocalTrack>),
    TogglePlay,
    Next,
    Previous,
    Seek(u32),
    SetVolume(f32),
    /// A volume while its slider is dragged: heard at once, saved on release.
    PreviewVolume(f32),
    ToggleMute,
    ToggleShuffle,
    CycleRepeat,
    /// Hands the bar back to Spotify.
    Close,
    /// Opens a folder in the system's file manager.
    OpenFolder(String),
    /// Appends songs to one of the account's Spotify playlists.
    AddToPlaylist {
        playlist_id: String,
        name: String,
        tracks: Vec<LocalTrack>,
    },
    /// Plays the files behind a Spotify playlist's local rows: `uris` in
    /// order, starting at `start`.
    PlayUris {
        uris: Vec<String>,
        start: String,
    },
    /// Plays a local row of a Spotify list, with the list's other local
    /// rows after it.
    PlayRow {
        context: RowContext,
        start: String,
    },
}

#[derive(Default)]
pub struct LocalFiles {
    loaded: bool,
    pub folders: Vec<String>,
    pub tracks: Vec<LocalTrack>,
    pub volume: f32,
    pub muted: bool,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub scan: Option<Arc<ScanProgress>>,
    picker: Option<Arc<Mutex<Option<Option<PathBuf>>>>>,
    player: Option<Player>,
    /// The bar and the playback keys belong to local files.
    pub active: bool,
    activated_at: Option<Instant>,
    pub status: Status,
    pub position_ms: u32,
    /// The song last asked for, shown until the player confirms it.
    expected: Option<(u64, LocalTrack, Instant)>,
    optimistic_playing: Option<(bool, Instant)>,
    generation: u64,
    save_generation: u64,
    /// The queue in its unshuffled order, so turning shuffle off restores it.
    base: Vec<LocalTrack>,
    pub filter: String,
    pub sort: LocalSort,
    pub sort_descending: bool,
    pub seek_preview: Option<f32>,
    pub volume_preview: Option<f32>,
    /// The playing song's cover, for the song start it belongs to.
    pub cover: Option<(u64, egui::TextureHandle)>,
    error_serial: u64,
    spotify_was_playing: bool,
    /// Local songs Spotify skipped in a playlist, gathered for a moment
    /// before they play here, and when the last skip came.
    handoff: Vec<LocalTrack>,
    handoff_at: Option<Instant>,
    /// Spotify continues when the handed-over songs end.
    resume_spotify: bool,
    /// Pushed by the views, applied after the frame.
    pub actions: Vec<LocalAction>,
    /// Counts changes to `tracks`, so the index below knows to rebuild.
    tracks_revision: u64,
    /// Songs by lowercased title, for finding a playlist's local rows.
    index: std::cell::RefCell<Option<(u64, HashMap<String, Vec<usize>>)>>,
}

impl LocalFiles {
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// The song the bar shows: the one just asked for until the player
    /// confirms it, then the player's.
    pub fn shown_track(&self) -> Option<LocalTrack> {
        if let Some((generation, track, at)) = &self.expected
            && self.status.generation < *generation
            && at.elapsed() < OPTIMISTIC_HOLD * 2
        {
            return Some(track.clone());
        }
        self.status.current.clone()
    }

    pub fn shown_playing(&self) -> bool {
        match self.optimistic_playing {
            Some((playing, at)) if at.elapsed() < OPTIMISTIC_HOLD => playing,
            _ => self.status.playing,
        }
    }

    /// Whether the song at `path` is the one playing (or just asked for).
    pub fn is_current(&self, path: &str) -> bool {
        self.active && self.shown_track().is_some_and(|track| track.path == path)
    }

    /// The position the bar shows, following a seek just asked for.
    pub fn shown_position_ms(&self) -> u32 {
        if self.expected.is_some() && self.status.generation < self.generation {
            return 0;
        }
        self.position_ms
    }

    /// The length of the song shown.
    pub fn shown_duration_ms(&self) -> u32 {
        match self.shown_track() {
            Some(track) if self.status.current.as_ref().map(|c| &c.path) == Some(&track.path) => {
                self.status.duration_ms.max(track.duration_ms)
            }
            Some(track) => track.duration_ms,
            None => 0,
        }
    }

    pub fn effective_volume(&self) -> f32 {
        if self.muted { 0.0 } else { self.volume }
    }

    /// The songs the page lists, filtered and sorted.
    /// The song in the library a `spotify:local:` URI describes: the same
    /// title, the same artist when the URI names one, and a length within
    /// three seconds when it gives one.
    pub fn find_uri(&self, uri: &str) -> Option<LocalTrack> {
        let (artist, album, title, seconds) = crate::extras::parse_local_uri(uri)?;
        let title = title.trim().to_lowercase();
        if title.is_empty() {
            return None;
        }
        let mut index = self.index.borrow_mut();
        if index
            .as_ref()
            .is_none_or(|(revision, _)| *revision != self.tracks_revision)
        {
            let mut map: HashMap<String, Vec<usize>> = HashMap::new();
            for (position, track) in self.tracks.iter().enumerate() {
                map.entry(track.title.trim().to_lowercase())
                    .or_default()
                    .push(position);
            }
            *index = Some((self.tracks_revision, map));
        }
        let (_, map) = index.as_ref()?;
        let artist = artist.trim().to_lowercase();
        let album = album.trim().to_lowercase();
        let candidates = map.get(&title)?;
        let fits = |track: &LocalTrack| {
            (artist.is_empty() || track.artist.trim().to_lowercase() == artist)
                && (seconds == 0
                    || track.duration_ms == 0
                    || (i64::from(track.duration_ms / 1000) - i64::from(seconds)).abs() <= 3)
        };
        let fitting: Vec<&LocalTrack> = candidates
            .iter()
            .map(|&position| &self.tracks[position])
            .filter(|track| fits(track))
            .collect();
        fitting
            .iter()
            .find(|track| track.album.trim().to_lowercase() == album)
            .or_else(|| fitting.first())
            .map(|track| (*track).clone())
    }

    pub fn visible_rows(&self) -> Vec<LocalTrack> {
        let needle = self.filter.trim().to_lowercase();
        let mut rows: Vec<LocalTrack> = self
            .tracks
            .iter()
            .filter(|track| needle.is_empty() || track.haystack().contains(&needle))
            .cloned()
            .collect();
        match self.sort {
            LocalSort::Artist => {}
            LocalSort::Title => rows.sort_by_cached_key(|track| track.title.to_lowercase()),
            LocalSort::Album => rows.sort_by_cached_key(|track| {
                (
                    track.album.to_lowercase(),
                    track.disc.unwrap_or(0),
                    track.track.unwrap_or(u32::MAX),
                )
            }),
            LocalSort::Duration => rows.sort_by_key(|track| track.duration_ms),
        }
        if self.sort_descending {
            rows.reverse();
        }
        rows
    }

    fn library_file(&self) -> LibraryFile {
        LibraryFile {
            folders: self.folders.clone(),
            tracks: self.tracks.clone(),
            volume: self.volume,
            shuffle: self.shuffle,
            repeat: self.repeat,
        }
    }
}

impl App {
    fn local_library_path(&self) -> PathBuf {
        self.dirs.config.join(crate::local_files::LIBRARY_FILE)
    }

    /// Reads the library the first time it is needed, then looks for new
    /// and changed files in the background.
    fn ensure_local_files_loaded(&mut self, ctx: &egui::Context) {
        if self.local_files.loaded {
            return;
        }
        let library = load_library(&self.local_library_path());
        let files = &mut self.local_files;
        files.loaded = true;
        files.folders = library.folders;
        files.tracks = library.tracks;
        files.tracks_revision += 1;
        files.volume = library.volume.clamp(0.0, 1.0);
        files.shuffle = library.shuffle;
        files.repeat = library.repeat;
        if !files.folders.is_empty() {
            self.rescan_local_files(ctx);
        }
    }

    fn save_local_files(&mut self) {
        self.local_files.save_generation += 1;
        save_library(
            self.local_library_path(),
            self.local_files.library_file(),
            self.local_files.save_generation,
        );
    }

    fn rescan_local_files(&mut self, ctx: &egui::Context) {
        if self
            .local_files
            .scan
            .as_ref()
            .is_some_and(|scan| !scan.finished.load(std::sync::atomic::Ordering::Acquire))
        {
            return;
        }
        self.local_files.scan = Some(start_scan(
            self.local_files.folders.clone(),
            self.local_files.tracks.clone(),
            ctx.clone(),
        ));
    }

    fn local_player(&mut self, ctx: &egui::Context) -> &Player {
        if self.local_files.player.is_none() {
            let player = Player::new(ctx.clone());
            player.set_volume(self.local_files.effective_volume());
            player.send(LocalCommand::SetRepeat(self.local_files.repeat));
            self.local_files.player = Some(player);
        }
        self.local_files
            .player
            .as_ref()
            .expect("the player was just made")
    }

    fn local_send(&mut self, ctx: &egui::Context, command: LocalCommand) {
        self.local_player(ctx).send(command);
    }

    /// Spotify is playing, here or on another device.
    fn spotify_playing(&self) -> bool {
        self.now_playing()
            .is_some_and(|now| now.playing && !now.resuming)
    }

    /// Gives the bar and the playback keys to local files, pausing Spotify.
    fn activate_local_files(&mut self) {
        if self.spotify_playing() {
            self.toggle_play();
        }
        if !self.local_files.active {
            self.local_files.active = true;
        }
        self.local_files.activated_at = Some(Instant::now());
        self.local_files.spotify_was_playing = false;
    }

    /// Hands the bar back to Spotify, pausing the local song.
    fn deactivate_local_files(&mut self) {
        if !self.local_files.active {
            return;
        }
        self.local_files.active = false;
        self.local_files.optimistic_playing = None;
        self.local_files.expected = None;
        if let Some(player) = &self.local_files.player {
            player.send(LocalCommand::Pause);
        }
    }

    /// Runs once a frame, after the frame's actions: applies what the
    /// views asked for and reads back what the player and scan did.
    pub(crate) fn local_files_frame(&mut self, ctx: &egui::Context) {
        // Read from the start: a Spotify playlist's local rows play from
        // the library, wherever they are opened.
        self.ensure_local_files_loaded(ctx);
        self.extras_frame();
        let actions = std::mem::take(&mut self.local_files.actions);
        for action in actions {
            self.apply_local_action(action, ctx);
        }
        self.poll_local_folder_picker(ctx);
        if let Some(scan) = &self.local_files.scan
            && let Some(tracks) = scan.take()
        {
            self.local_files.scan = None;
            if tracks != self.local_files.tracks {
                self.local_files.tracks = tracks;
                self.local_files.tracks_revision += 1;
                self.save_local_files();
            }
        }
        self.poll_local_player(ctx);
        self.local_handoff_frame(ctx);
        // Spotify starting on its own (from a phone, the tray or Connect)
        // takes the bar back.
        let spotify_playing = self.spotify_playing();
        let settled = self
            .local_files
            .activated_at
            .is_none_or(|at| at.elapsed() > SWITCH_GRACE);
        if self.local_files.active
            && spotify_playing
            && !self.local_files.spotify_was_playing
            && settled
        {
            self.deactivate_local_files();
        }
        self.local_files.spotify_was_playing = spotify_playing;
        if self.local_files.active && self.local_files.shown_playing() {
            theme::repaint_after(ctx, Duration::from_millis(250));
        }
    }

    /// Spotify reached a playlist's local file, which it cannot play: it
    /// skips it, and the song plays here instead when Local Files has it.
    /// Skips that come together (several local songs in a row) play as one
    /// run, then Spotify carries on.
    pub(crate) fn local_handoff(&mut self, uri: &str) {
        let Some(track) = self.local_files.find_uri(uri) else {
            let title = crate::extras::parse_local_uri(uri)
                .map(|(_, _, title, _)| title)
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| "a local song".into());
            self.toast(format!(
                "Skipped {title}: it isn't in your Local Files folders."
            ));
            return;
        };
        if self.spotify_playing() {
            self.toggle_play();
        }
        self.local_files.handoff.push(track);
        self.local_files.handoff_at = Some(Instant::now());
    }

    fn local_handoff_frame(&mut self, ctx: &egui::Context) {
        if let Some(at) = self.local_files.handoff_at {
            if at.elapsed() < HANDOFF_GATHER {
                theme::repaint_after(ctx, HANDOFF_GATHER);
            } else {
                self.local_files.handoff_at = None;
                let rows = std::mem::take(&mut self.local_files.handoff);
                if self.spotify_playing() {
                    self.toggle_play();
                }
                self.play_local_rows(rows, 0, ctx);
                self.local_files.resume_spotify = true;
            }
        }
        if self.local_files.resume_spotify
            && self.local_files.active
            && self.local_files.expected.is_none()
            && self.local_files.status.finished
        {
            self.local_files.resume_spotify = false;
            self.deactivate_local_files();
            if !self.spotify_playing() {
                self.toggle_play();
            }
        }
    }

    fn poll_local_player(&mut self, ctx: &egui::Context) {
        let Some(player) = &self.local_files.player else {
            return;
        };
        let status = player.status();
        let position = player.position_ms();
        let serial = status.track_serial;
        let cover = if self
            .local_files
            .cover
            .as_ref()
            .is_none_or(|(shown, _)| *shown != serial)
        {
            player.take_cover(serial)
        } else {
            None
        };
        if let Some(cover) = cover {
            self.local_files.cover = cover.map(|cover| {
                let pixels = cover
                    .rgba
                    .chunks_exact(4)
                    .map(|p| egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
                    .collect();
                let texture = ctx.load_texture(
                    "local-files-cover",
                    egui::ColorImage::new(cover.size, pixels),
                    egui::TextureOptions::LINEAR,
                );
                (serial, texture)
            });
        }
        if let Some((error_serial, message)) = &status.error
            && *error_serial > self.local_files.error_serial
        {
            self.local_files.error_serial = *error_serial;
            let message = message.clone();
            self.toast_error(message);
        }
        if let Some((generation, _, _)) = &self.local_files.expected
            && status.generation >= *generation
        {
            self.local_files.expected = None;
        }
        if let Some((playing, _)) = self.local_files.optimistic_playing
            && playing == status.playing
        {
            self.local_files.optimistic_playing = None;
        }
        self.local_files.status = status;
        self.local_files.position_ms = position;
    }

    fn poll_local_folder_picker(&mut self, ctx: &egui::Context) {
        let picked = match &self.local_files.picker {
            Some(slot) => slot.lock().unwrap_or_else(PoisonError::into_inner).take(),
            None => return,
        };
        let Some(picked) = picked else {
            return;
        };
        self.local_files.picker = None;
        if let Some(path) = picked.and_then(|path| path.to_str().map(str::to_string)) {
            self.add_local_folder(path, ctx);
        }
    }

    fn add_local_folder(&mut self, path: String, ctx: &egui::Context) {
        if !self.local_files.folders.contains(&path) {
            self.local_files.folders.push(path);
            self.save_local_files();
        }
        self.rescan_local_files(ctx);
    }

    /// Opens the system's folder chooser. It is made here, on the UI
    /// thread, and waited for on its own thread.
    fn pick_local_folder(&mut self, ctx: &egui::Context) {
        if self.local_files.picker.is_some() {
            return;
        }
        let mut dialog = rfd::AsyncFileDialog::new().set_title("Add a music folder");
        if let Some(music) = directories::UserDirs::new()
            .and_then(|dirs| dirs.audio_dir().map(std::path::Path::to_path_buf))
        {
            dialog = dialog.set_directory(music);
        }
        let chosen = dialog.pick_folder();
        let slot = Arc::new(Mutex::new(None));
        let result = Arc::clone(&slot);
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("local-files-picker".into())
            .spawn(move || {
                let picked = match tokio::runtime::Builder::new_current_thread().build() {
                    Ok(runtime) => runtime.block_on(chosen),
                    Err(error) => {
                        log::warn!("local files: cannot wait for the folder chooser: {error}");
                        None
                    }
                };
                *result.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(picked.map(|handle| handle.path().to_path_buf()));
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            self.local_files.picker = Some(slot);
        }
    }

    fn apply_local_action(&mut self, action: LocalAction, ctx: &egui::Context) {
        match action {
            LocalAction::AddFolder => self.pick_local_folder(ctx),
            LocalAction::AddFolderPath(path) => self.add_local_folder(path, ctx),
            LocalAction::RemoveFolder(path) => {
                self.local_files.folders.retain(|folder| folder != &path);
                let prefix = std::path::Path::new(&path);
                self.local_files
                    .tracks
                    .retain(|track| !std::path::Path::new(&track.path).starts_with(prefix));
                self.local_files.tracks_revision += 1;
                self.save_local_files();
                if !self.local_files.folders.is_empty() {
                    self.rescan_local_files(ctx);
                }
            }
            LocalAction::Rescan => self.rescan_local_files(ctx),
            LocalAction::Play { rows, index } => self.play_local_rows(rows, index, ctx),
            LocalAction::ShuffleAll(rows) => {
                if rows.is_empty() {
                    return;
                }
                if !self.local_files.shuffle {
                    self.local_files.shuffle = true;
                    self.save_local_files();
                }
                let index = rand::random_range(0..rows.len());
                self.play_local_rows(rows, index, ctx);
            }
            LocalAction::TogglePlay => self.toggle_local_play(ctx),
            LocalAction::Next => {
                self.local_files.optimistic_playing = Some((true, Instant::now()));
                self.local_send(ctx, LocalCommand::Next);
            }
            LocalAction::Previous => self.local_send(ctx, LocalCommand::Previous),
            LocalAction::Seek(ms) => {
                self.local_files.position_ms = ms;
                self.local_send(ctx, LocalCommand::Seek(ms));
            }
            LocalAction::SetVolume(level) => {
                self.local_files.volume = level.clamp(0.0, 1.0);
                self.local_files.muted = false;
                let volume = self.local_files.effective_volume();
                self.local_player(ctx).set_volume(volume);
                self.save_local_files();
            }
            LocalAction::PreviewVolume(level) => {
                self.local_files.volume = level.clamp(0.0, 1.0);
                self.local_files.muted = false;
                let volume = self.local_files.effective_volume();
                self.local_player(ctx).set_volume(volume);
            }
            LocalAction::ToggleMute => {
                self.local_files.muted = !self.local_files.muted;
                let volume = self.local_files.effective_volume();
                self.local_player(ctx).set_volume(volume);
            }
            LocalAction::ToggleShuffle => self.set_local_shuffle(!self.local_files.shuffle, ctx),
            LocalAction::CycleRepeat => {
                self.local_files.repeat = self.local_files.repeat.next();
                let repeat = self.local_files.repeat;
                self.local_send(ctx, LocalCommand::SetRepeat(repeat));
                self.save_local_files();
            }
            LocalAction::Close => self.deactivate_local_files(),
            LocalAction::OpenFolder(folder) => {
                if let Err(error) = open::that_detached(&folder) {
                    self.toast_error(format!("Couldn't open the folder: {error}"));
                }
            }
            LocalAction::AddToPlaylist {
                playlist_id,
                name,
                tracks,
            } => {
                let uris = tracks
                    .iter()
                    .map(|track| {
                        crate::extras::local_uri(
                            &track.artist,
                            &track.album,
                            &track.title,
                            track.duration_ms,
                        )
                    })
                    .collect();
                self.add_local_to_playlist(playlist_id, name, uris);
            }
            LocalAction::PlayRow { context, start } => {
                let mut uris = self.local_uris_in(&context);
                // The row always plays, even when its list is not loaded.
                if !uris.contains(&start) {
                    uris = vec![start.clone()];
                }
                self.apply_local_action(LocalAction::PlayUris { uris, start }, ctx);
            }
            LocalAction::PlayUris { uris, start } => {
                let mut rows = Vec::new();
                let mut index = None;
                for uri in &uris {
                    if let Some(track) = self.local_files.find_uri(uri) {
                        if *uri == start && index.is_none() {
                            index = Some(rows.len());
                        }
                        rows.push(track);
                    }
                }
                match index {
                    Some(index) => self.play_local_rows(rows, index, ctx),
                    None => self.toast(
                        "This song isn't in your Local Files folders. Add its folder in Local Files.",
                    ),
                }
            }
        }
    }

    /// The `spotify:local:` URIs among a list's rows, in order: what plays
    /// from the library when one of them is double-clicked.
    pub(crate) fn local_uris_in(&self, context: &RowContext) -> Vec<String> {
        let local = |uri: &String| uri.starts_with("spotify:local:");
        match context {
            RowContext::Uris(uris) | RowContext::View { uris, .. } => {
                uris.iter().filter(|uri| local(uri)).cloned().collect()
            }
            RowContext::Context { uri, .. } => uri
                .strip_prefix("spotify:playlist:")
                .and_then(|id| self.playlist_pages.get(id))
                .map(|page| {
                    page.items
                        .items
                        .iter()
                        .filter_map(|row| row.playable().map(|item| item.uri().to_string()))
                        .filter(|uri| local(uri))
                        .collect()
                })
                .unwrap_or_default(),
            RowContext::Queue => Vec::new(),
        }
    }

    fn play_local_rows(&mut self, rows: Vec<LocalTrack>, index: usize, ctx: &egui::Context) {
        let Some(chosen) = rows.get(index).cloned() else {
            return;
        };
        self.local_files.resume_spotify = false;
        self.local_files.base = rows.clone();
        let (queue, start) = if self.local_files.shuffle {
            (shuffled_from(&rows, index), 0)
        } else {
            (rows, index)
        };
        self.local_files.generation += 1;
        let generation = self.local_files.generation;
        self.local_files.expected = Some((generation, chosen, Instant::now()));
        self.local_files.optimistic_playing = Some((true, Instant::now()));
        self.local_files.position_ms = 0;
        self.activate_local_files();
        self.local_send(
            ctx,
            LocalCommand::Load {
                queue,
                index: start,
                generation,
            },
        );
    }

    fn toggle_local_play(&mut self, ctx: &egui::Context) {
        if self.local_files.shown_track().is_none() {
            // Nothing loaded: play the list from the top.
            let rows = self.local_files.visible_rows();
            if rows.is_empty() {
                self.toast("Add a folder of music to play local files");
            } else {
                self.play_local_rows(rows, 0, ctx);
            }
            return;
        }
        let playing = !self.local_files.shown_playing();
        self.local_files.optimistic_playing = Some((playing, Instant::now()));
        if playing {
            self.activate_local_files();
            self.local_send(ctx, LocalCommand::Resume);
        } else {
            self.local_send(ctx, LocalCommand::Pause);
        }
    }

    fn set_local_shuffle(&mut self, shuffle: bool, ctx: &egui::Context) {
        self.local_files.shuffle = shuffle;
        self.save_local_files();
        let Some(current) = self.local_files.status.current.clone() else {
            return;
        };
        let base = &self.local_files.base;
        let Some(at) = base.iter().position(|track| track.path == current.path) else {
            return;
        };
        let (queue, index) = if shuffle {
            (shuffled_from(base, at), 0)
        } else {
            (base.clone(), at)
        };
        self.local_send(ctx, LocalCommand::Reorder { queue, index });
    }

    /// Spotify's playback actions while local files have the bar. Returns
    /// the action when Spotify should still handle it.
    pub(crate) fn local_files_take(
        &mut self,
        action: Action,
        ctx: &egui::Context,
    ) -> Option<Action> {
        if !self.local_files.active {
            return Some(action);
        }
        let duration = self.local_files.shown_duration_ms();
        let local = match action {
            Action::TogglePlay => LocalAction::TogglePlay,
            Action::Next => LocalAction::Next,
            Action::Previous => LocalAction::Previous,
            Action::Seek(ms) if duration > 0 => LocalAction::Seek(ms.min(duration)),
            Action::Seek(ms) => LocalAction::Seek(ms),
            Action::SeekBy(delta) => {
                let target = i64::from(self.local_files.position_ms) + delta;
                LocalAction::Seek(target.clamp(0, i64::from(duration)) as u32)
            }
            Action::SetVolume(percent) => LocalAction::SetVolume(f32::from(percent) / 100.0),
            Action::PreviewVolume(percent) => {
                LocalAction::PreviewVolume(f32::from(percent) / 100.0)
            }
            Action::VolumeBy(step) => {
                LocalAction::SetVolume(self.local_files.volume + f32::from(step) / 100.0)
            }
            Action::ToggleMute => LocalAction::ToggleMute,
            Action::ToggleShuffle => LocalAction::ToggleShuffle,
            Action::SetShuffle(shuffle) => {
                self.set_local_shuffle(shuffle, ctx);
                return None;
            }
            Action::CycleRepeat => LocalAction::CycleRepeat,
            Action::SetRepeat(mode) => {
                self.local_files.repeat = match mode {
                    crate::player::RepeatMode::Off => Repeat::Off,
                    crate::player::RepeatMode::Context => Repeat::All,
                    crate::player::RepeatMode::Track => Repeat::One,
                };
                let repeat = self.local_files.repeat;
                self.local_send(ctx, LocalCommand::SetRepeat(repeat));
                return None;
            }
            // Playing anything from Spotify gives the bar back to it.
            action @ (Action::PlayContext { .. }
            | Action::PlaySearch { .. }
            | Action::PlaySearchMatch { .. }
            | Action::PlayEpisode { .. }
            | Action::PlayUris { .. }
            | Action::PlayFromRow { .. }
            | Action::ShufflePlay(_)
            | Action::Transfer(_)) => {
                self.deactivate_local_files();
                return Some(action);
            }
            other => return Some(other),
        };
        self.apply_local_action(local, ctx);
        None
    }
}

/// `rows` shuffled, with `rows[first]` leading.
fn shuffled_from(rows: &[LocalTrack], first: usize) -> Vec<LocalTrack> {
    use rand::seq::SliceRandom;
    let mut rest: Vec<LocalTrack> = rows
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != first)
        .map(|(_, track)| track.clone())
        .collect();
    rest.shuffle(&mut rand::rng());
    let mut queue = Vec::with_capacity(rows.len());
    if let Some(track) = rows.get(first) {
        queue.push(track.clone());
    }
    queue.extend(rest);
    queue
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(path: &str) -> LocalTrack {
        LocalTrack {
            path: path.into(),
            title: path.into(),
            ..LocalTrack::default()
        }
    }

    #[test]
    fn shuffling_keeps_the_chosen_song_first_and_every_song_once() {
        let rows: Vec<_> = ["a", "b", "c", "d", "e"].into_iter().map(track).collect();
        let queue = shuffled_from(&rows, 2);
        assert_eq!(queue[0].path, "c");
        let mut paths: Vec<_> = queue.iter().map(|track| track.path.clone()).collect();
        paths.sort();
        assert_eq!(paths, vec!["a", "b", "c", "d", "e"]);
    }

    #[test]
    fn the_filter_matches_title_artist_and_album() {
        let mut files = LocalFiles::default();
        files.tracks = vec![
            LocalTrack {
                artist: "Nina Simone".into(),
                ..track("one")
            },
            LocalTrack {
                album: "Blue Train".into(),
                ..track("two")
            },
        ];
        files.filter = "simone".into();
        assert_eq!(files.visible_rows().len(), 1);
        files.filter = "TRAIN".into();
        assert_eq!(files.visible_rows()[0].path, "two");
        files.filter.clear();
        assert_eq!(files.visible_rows().len(), 2);
    }
}

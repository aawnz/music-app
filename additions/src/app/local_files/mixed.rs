//! A Spotify playlist with local files, played in an order this app keeps.
//!
//! librespot plays Spotify's songs but drops local files from its queue,
//! and keeps its shuffle order to itself. For a playlist that holds local
//! files, the app therefore decides the order (shuffled or not) and plays
//! each stretch with the right player: a run of Spotify songs is handed to
//! librespot as a plain, unshuffled list; each local song plays here. When
//! one ends, the next item in the order starts, whichever player it needs.

use super::*;

/// What the order does next, taken on the next frame.
enum Step {
    Play(usize),
}

pub struct MixedQueue {
    /// The playlist (or view) the order came from, shown as playing.
    context_uri: Option<String>,
    /// The list as the playlist holds it.
    original: Vec<String>,
    /// The play order: `original`, or shuffled.
    order: Vec<String>,
    index: usize,
    pub shuffle: bool,
    /// The Spotify songs handed to librespot: `order[start..end]`.
    segment: Option<(usize, usize)>,
    /// When the segment's last song was last seen in its final seconds.
    near_end_at: Option<Instant>,
    /// A local song of the order is playing here.
    local_playing: bool,
    pending: Option<Step>,
}

/// Previous restarts a song played longer than this, as Spotify does.
const RESTART_AFTER_MS: u32 = 3_000;
/// A song this close to its end when the next starts ended on its own.
const END_SLACK_MS: u32 = 5_000;
/// How long that sighting counts while Spotify moves on.
const END_MEMORY: Duration = Duration::from_secs(15);

fn is_local(uri: &str) -> bool {
    uri.starts_with("spotify:local:")
}

/// `items` shuffled, with `first` (when given) kept at the front.
fn shuffled(mut items: Vec<String>, first: Option<String>) -> Vec<String> {
    use rand::seq::SliceRandom;
    items.shuffle(&mut rand::rng());
    if let Some(first) = first
        && let Some(position) = items.iter().position(|uri| *uri == first)
    {
        let first = items.remove(position);
        items.insert(0, first);
    }
    items
}

impl App {
    pub(crate) fn mixed_active(&self) -> bool {
        self.local_files.mixed.is_some()
    }

    /// The shuffle shown while the order is the app's.
    pub fn mixed_shuffle(&self) -> Option<bool> {
        self.local_files.mixed.as_ref().map(|mixed| mixed.shuffle)
    }

    /// A list's rows when it holds local files this computer has, and all of
    /// it is loaded: what the app's order is made from.
    fn mixed_list(&self, context: &RowContext) -> Option<Vec<String>> {
        if !matches!(self.target(), Target::Local) {
            return None;
        }
        if let RowContext::Context { uri, .. } = context {
            let Some(page) = uri
                .strip_prefix("spotify:playlist:")
                .and_then(|id| self.playlist_pages.get(id))
            else {
                log::info!("mixed: {uri} is not open, so its order is Spotify's");
                return None;
            };
            let loaded = page.items.base_offset == 0
                && page
                    .items
                    .total
                    .is_none_or(|total| total as usize <= page.items.items.len());
            if !loaded {
                log::info!(
                    "mixed: {uri} is not all loaded (from {}, {} of {:?})",
                    page.items.base_offset,
                    page.items.items.len(),
                    page.items.total
                );
                return None;
            }
        }
        let rows = self.list_rows(context);
        let locals = rows.iter().filter(|uri| is_local(uri)).count();
        let found = rows
            .iter()
            .filter(|uri| is_local(uri) && self.local_files.find_uri(uri).is_some())
            .count();
        if locals > 0 {
            log::info!(
                "mixed: list of {} rows, {locals} local, {found} found here",
                rows.len()
            );
        }
        (found > 0).then_some(rows)
    }

    fn context_uri_of(context: &RowContext) -> Option<String> {
        match context {
            RowContext::Context { uri, .. } => Some(uri.clone()),
            RowContext::View { context_uri, .. } => Some(context_uri.clone()),
            RowContext::Uris(_) | RowContext::Queue => None,
        }
    }

    /// Starts the app's order for a list, at `start` (a row index) or, with
    /// none, at the top (or anywhere, shuffled).
    fn start_mixed(
        &mut self,
        context_uri: Option<String>,
        rows: Vec<String>,
        start: Option<usize>,
        shuffle: bool,
        ctx: &egui::Context,
    ) {
        let start = start.filter(|&index| index < rows.len());
        log::info!("mixed: start at {start:?}, shuffle {shuffle}");
        let (order, index) = if shuffle {
            let first = start.map(|index| rows[index].clone());
            (shuffled(rows.clone(), first), 0)
        } else {
            (rows.clone(), start.unwrap_or(0))
        };
        // The local player's own repeat would loop a song the order means
        // to move past; the order repeats by Spotify's setting instead.
        if self.local_files.repeat != Repeat::Off {
            self.local_send(ctx, LocalCommand::SetRepeat(Repeat::Off));
        }
        self.local_files.plan = None;
        self.local_files.pending_run = None;
        self.local_files.mixed = Some(MixedQueue {
            context_uri,
            original: rows,
            order,
            index,
            shuffle,
            segment: None,
            near_end_at: None,
            local_playing: false,
            pending: None,
        });
        self.mixed_play(index, 0, ctx);
    }

    /// Leaves the app's order; what plays next is up to whoever asked.
    pub(crate) fn end_mixed(&mut self, ctx: &egui::Context) {
        if self.local_files.mixed.is_some() {
            log::info!("mixed: left the order");
        }
        if self.local_files.mixed.take().is_some() && self.local_files.repeat != Repeat::Off {
            let repeat = self.local_files.repeat;
            self.local_send(ctx, LocalCommand::SetRepeat(repeat));
        }
    }

    /// Plays the order from `index`: a local song here, or the Spotify
    /// songs from there to the next local song through librespot.
    fn mixed_play(&mut self, index: usize, position_ms: u32, ctx: &egui::Context) {
        let Some(mixed) = self.local_files.mixed.as_ref() else {
            return;
        };
        if index >= mixed.order.len() {
            self.mixed_finish(ctx);
            return;
        }
        let uri = mixed.order[index].clone();
        log::info!(
            "mixed: play {index} of {} ({})",
            mixed.order.len(),
            if is_local(&uri) { "local" } else { "Spotify" }
        );
        if is_local(&uri) {
            let Some(track) = self.local_files.find_uri(&uri) else {
                log::info!("mixed: local song {index} is not in the library: skipped");
                // Not on this computer: skipped, as Spotify skips it.
                self.mixed_play(index + 1, 0, ctx);
                return;
            };
            if self.spotify_playing() {
                self.toggle_play();
            }
            // One song at a time, so the order decides what follows it.
            self.play_local_rows(vec![track], 0, ctx);
            if let Some(mixed) = self.local_files.mixed.as_mut() {
                mixed.index = index;
                mixed.segment = None;
                mixed.local_playing = true;
                mixed.near_end_at = None;
            }
            return;
        }
        let end = (index..mixed.order.len())
            .find(|&at| is_local(&mixed.order[at]))
            .unwrap_or(mixed.order.len());
        let uris = mixed.order[index..end].to_vec();
        let context_uri = mixed.context_uri.clone();
        let shuffle = mixed.shuffle;
        self.deactivate_local_files();
        self.load_spotify_stretch(uris, position_ms, context_uri, shuffle);
        if let Some(mixed) = self.local_files.mixed.as_mut() {
            mixed.index = index;
            mixed.segment = Some((index, end));
            mixed.local_playing = false;
            mixed.near_end_at = None;
        }
        ctx.request_repaint();
    }

    /// Hands librespot a plain list of Spotify songs, never shuffled (the
    /// order is the app's) and with no Autoplay after it (the order goes on).
    fn load_spotify_stretch(
        &mut self,
        uris: Vec<String>,
        position_ms: u32,
        context_uri: Option<String>,
        shuffle: bool,
    ) {
        let mut request = PlayRequest::tracks(uris).starting_at_index(0);
        request.position_ms = position_ms;
        self.supersede_play_search();
        if let Some(first) = request.uris.first().cloned() {
            self.expect_track(first.clone(), position_ms);
            self.set_play_pending(vec![first]);
        }
        self.assumed_context = Some(AssumedContext {
            uri: context_uri.unwrap_or_default(),
            shuffle: shuffle.then_some(true),
            at: Instant::now(),
        });
        let mut load = local_load(&request, false);
        load.repeat = Some(self.local.repeat);
        self.local_list = None;
        self.backend.player(PlayerCommand::Load(load));
        if self.local.shuffle {
            self.backend.player(PlayerCommand::Shuffle(false));
        }
        self.optimistic_playing = Some((true, Instant::now()));
    }

    /// The end of the order: around again with repeat, else Autoplay when
    /// it is on, else everything stays paused.
    fn mixed_finish(&mut self, ctx: &egui::Context) {
        log::info!("mixed: the order ended");
        let Some(mixed) = self.local_files.mixed.as_ref() else {
            return;
        };
        if self.local.repeat == crate::player::RepeatMode::Context && !mixed.order.is_empty() {
            self.mixed_play(0, 0, ctx);
            return;
        }
        let seed = mixed.order.iter().rev().find(|uri| !is_local(uri)).cloned();
        self.end_mixed(ctx);
        self.deactivate_local_files();
        if self.settings.autoplay
            && let Some(seed) = seed
        {
            self.backend.player(PlayerCommand::Load(LoadSpec {
                context_uri: Some(seed),
                play: true,
                autoplay: true,
                ..LoadSpec::default()
            }));
        }
    }

    /// Next from either bar.
    fn mixed_next(&mut self, ctx: &egui::Context) {
        if let Some(index) = self.local_files.mixed.as_ref().map(|mixed| mixed.index) {
            self.mixed_play(index + 1, 0, ctx);
        }
    }

    /// Previous from either bar: the start of the song, or the one before.
    fn mixed_previous(&mut self, ctx: &egui::Context) {
        let Some((index, local_playing)) = self
            .local_files
            .mixed
            .as_ref()
            .map(|mixed| (mixed.index, mixed.local_playing))
        else {
            return;
        };
        let position = if local_playing {
            self.local_files.position_ms
        } else {
            self.local.position_now()
        };
        if position > RESTART_AFTER_MS || index == 0 {
            if local_playing {
                self.local_files.position_ms = 0;
                self.local_send(ctx, LocalCommand::Seek(0));
            } else {
                self.actions.push(Action::Seek(0));
            }
            return;
        }
        self.mixed_play(index - 1, 0, ctx);
    }

    /// Shuffle switched on or off: the songs after this one take the new
    /// order, as in Spotify.
    fn mixed_set_shuffle(&mut self, shuffle: bool, ctx: &egui::Context) {
        let Some(mixed) = self.local_files.mixed.as_mut() else {
            return;
        };
        self.shuffle_wanted = shuffle;
        self.shuffle_set_at = Some(Instant::now());
        if mixed.shuffle == shuffle {
            return;
        }
        let current = mixed.order.get(mixed.index).cloned();
        mixed.shuffle = shuffle;
        if shuffle {
            mixed.order = shuffled(mixed.original.clone(), current.clone());
            mixed.index = 0;
        } else {
            mixed.order = mixed.original.clone();
            mixed.index = current
                .as_ref()
                .and_then(|current| mixed.order.iter().position(|uri| uri == current))
                .unwrap_or(0);
        }
        // librespot holds the old order of the Spotify songs after this
        // one: hand it the new stretch from where this song is.
        if !mixed.local_playing {
            let index = mixed.index;
            let position = self.local.position_now();
            self.mixed_play(index, position, ctx);
        }
    }

    /// Follows Spotify's updates while it plays a stretch of the order.
    pub(crate) fn mixed_track(&mut self, state: &LocalState) {
        let Some(mixed) = self.local_files.mixed.as_ref() else {
            return;
        };
        let Some((start, end)) = mixed.segment else {
            return;
        };
        let last = mixed.order[end - 1].clone();
        let near_end = |seen: &LocalState| {
            seen.playback == Playback::Playing
                && seen.track.as_ref().is_some_and(|track| {
                    track.uri == last
                        && track.duration_ms > 0
                        && seen.position_now().saturating_add(END_SLACK_MS) >= track.duration_ms
                })
        };
        let seen_near_end = near_end(&self.local) || near_end(state);
        let index = mixed.index;
        log::info!(
            "mixed: Spotify {:?} {} at {}ms (place {index} in {start}..{end}, near end {seen_near_end})",
            state.playback,
            state.track.as_ref().map_or("-", |track| track.uri.as_str()),
            state.position_now()
        );
        let current = state.track.as_ref().map(|track| track.uri.clone());
        let previous = self.local.track.as_ref().map(|track| track.uri.clone());
        let Some(mixed) = self.local_files.mixed.as_mut() else {
            return;
        };
        if seen_near_end && index == end - 1 {
            mixed.near_end_at = Some(Instant::now());
        }
        let ended = mixed
            .near_end_at
            .is_some_and(|at| at.elapsed() < END_MEMORY);
        // The stretch's last song ended: Spotify stopped after it, or went
        // around (its repeat). The order goes on from the next item.
        let stopped = matches!(state.playback, Playback::Stopped | Playback::Paused)
            && state.position_ms < END_SLACK_MS;
        let changed = current.is_some() && current != previous;
        if ended && index == end - 1 && (stopped || changed) {
            log::info!("mixed: the Spotify stretch ended; next is {end}");
            mixed.near_end_at = None;
            mixed.segment = None;
            mixed.pending = Some(Step::Play(end));
            return;
        }
        // Spotify moved to the next song of the stretch.
        if changed && let Some(current) = current {
            if let Some(at) = (index + 1..end).find(|&at| mixed.order[at] == current) {
                mixed.index = at;
            } else if (start..end).any(|at| mixed.order[at] == current) {
                // A song played again (repeat one): same place.
            } else if index + 1 < end {
                // A relinked song reports another URI: the next place.
                mixed.index = index + 1;
            }
        }
    }

    /// Runs once a frame: takes the pending step, and moves on when a local
    /// song of the order ends.
    pub(crate) fn mixed_frame(&mut self, ctx: &egui::Context) {
        let Some(mixed) = self.local_files.mixed.as_mut() else {
            return;
        };
        if let Some(Step::Play(index)) = mixed.pending.take() {
            self.mixed_play(index, 0, ctx);
            return;
        }
        let Some(mixed) = self.local_files.mixed.as_ref() else {
            return;
        };
        let (local_playing, index) = (mixed.local_playing, mixed.index);
        if local_playing
            && self.local_files.active
            && self.local_files.expected.is_none()
            && self.local_files.status.finished
        {
            // Repeat one plays the song again; otherwise the next item.
            let next = if self.local.repeat == crate::player::RepeatMode::Track {
                index
            } else {
                index + 1
            };
            self.mixed_play(next, 0, ctx);
        } else if local_playing && !self.local_files.active {
            // Spotify took the bar back without the order (a phone, the
            // tray): the order is over.
            self.end_mixed(ctx);
        }
    }

    /// Whether an action belongs to the app's order, and takes it if so.
    /// Starting a list with local files enters the order; playing anything
    /// else leaves it.
    pub(crate) fn mixed_take(&mut self, action: Action, ctx: &egui::Context) -> Option<Action> {
        match action {
            Action::PlayFromRow {
                ref context,
                ref uri,
                index,
            } if !matches!(context, RowContext::Queue) => {
                if let Some(rows) = self.mixed_list(context) {
                    let start = if rows.get(index as usize) == Some(uri) {
                        Some(index as usize)
                    } else {
                        rows.iter().position(|held| held == uri)
                    };
                    let shuffle = self.shuffle_wanted;
                    self.start_mixed(Self::context_uri_of(context), rows, start, shuffle, ctx);
                    return None;
                }
            }
            Action::PlayContext {
                ref uri,
                ref offset_uri,
                offset_index,
            } => {
                let context = RowContext::Context {
                    uri: uri.clone(),
                    editable_playlist: None,
                };
                if let Some(rows) = self.mixed_list(&context) {
                    let start = offset_uri
                        .as_ref()
                        .and_then(|offset| rows.iter().position(|held| held == offset))
                        .or(offset_index.map(|index| index as usize));
                    let shuffle = self.shuffle_wanted;
                    self.start_mixed(Some(uri.clone()), rows, start, shuffle, ctx);
                    return None;
                }
            }
            Action::ShufflePlay(ref uri) => {
                let context = RowContext::Context {
                    uri: uri.clone(),
                    editable_playlist: None,
                };
                if let Some(rows) = self.mixed_list(&context) {
                    self.shuffle_wanted = true;
                    self.shuffle_set_at = Some(Instant::now());
                    self.start_mixed(Some(uri.clone()), rows, None, true, ctx);
                    return None;
                }
            }
            _ => {}
        }
        if !self.mixed_active() {
            return Some(action);
        }
        match action {
            Action::Next => {
                self.mixed_next(ctx);
                None
            }
            Action::Previous => {
                self.mixed_previous(ctx);
                None
            }
            Action::ToggleShuffle => {
                let shuffle = !self.mixed_shuffle().unwrap_or(false);
                self.mixed_set_shuffle(shuffle, ctx);
                None
            }
            Action::SetShuffle(shuffle) => {
                self.mixed_set_shuffle(shuffle, ctx);
                None
            }
            action @ (Action::PlayContext { .. }
            | Action::PlaySearch { .. }
            | Action::PlaySearchMatch { .. }
            | Action::PlayEpisode { .. }
            | Action::PlayUris { .. }
            | Action::PlayFromRow { .. }
            | Action::ShufflePlay(_)
            | Action::Transfer(_)) => {
                self.end_mixed(ctx);
                Some(action)
            }
            action => Some(action),
        }
    }

    /// The local bar's buttons while a local song of the order plays.
    pub(crate) fn mixed_take_local(&mut self, action: &LocalAction, ctx: &egui::Context) -> bool {
        if !self.mixed_active() {
            return false;
        }
        match action {
            LocalAction::Next => self.mixed_next(ctx),
            LocalAction::Previous => self.mixed_previous(ctx),
            LocalAction::ToggleShuffle => {
                let shuffle = !self.mixed_shuffle().unwrap_or(false);
                self.mixed_set_shuffle(shuffle, ctx);
            }
            // One repeat for the whole order: Spotify's.
            LocalAction::CycleRepeat => {
                self.actions.push(Action::CycleRepeat);
                ctx.request_repaint();
            }
            LocalAction::Play { .. } | LocalAction::ShuffleAll(_) | LocalAction::Close => {
                self.end_mixed(ctx);
                return false;
            }
            _ => return false,
        }
        true
    }

    /// A local row of a list double-clicked: the order from that row when
    /// the list can have one.
    pub(crate) fn mixed_play_row(
        &mut self,
        context: &RowContext,
        start: &str,
        ctx: &egui::Context,
    ) -> bool {
        let Some(rows) = self.mixed_list(context) else {
            return false;
        };
        let Some(index) = rows.iter().position(|uri| uri == start) else {
            return false;
        };
        let shuffle = self.shuffle_wanted;
        self.start_mixed(
            Self::context_uri_of(context),
            rows,
            Some(index),
            shuffle,
            ctx,
        );
        true
    }
}

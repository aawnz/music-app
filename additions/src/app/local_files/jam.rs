//! Spotifast Jam: listening together with friends who use Spotifast.
//!
//! One app hosts and shares a code; others join with it. The host tells the
//! relay what it plays (song, place, playing or paused) and everyone else's
//! app plays the same, each from its own Spotify account. Guests' controls
//! (play, pause, skip, seek, picking a song) go to the host, who applies
//! them; songs anyone adds go to the host's queue through the relay, which
//! keeps the shared list of who added what.

use super::*;
use crate::jam::{Connection, Incoming, clean_code, new_code, now_ms};
use serde_json::{Value, json};

/// A guest corrects its place when it is this far from the host's.
const DRIFT_MS: i64 = 2_500;
/// After acting on the host's state, a guest waits this long for its
/// player to catch up before it corrects again.
const SETTLE: Duration = Duration::from_secs(3);
/// The host repeats its state this often, so drift is caught.
const HEARTBEAT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Default)]
pub struct Member {
    pub id: String,
    pub name: String,
    pub host: bool,
}

#[derive(Clone, Debug, Default)]
pub struct QueueItem {
    pub id: String,
    pub uri: String,
    pub title: String,
    pub artist: String,
    pub by: String,
}

/// What the host plays, as the relay last said.
#[derive(Clone, Debug, Default)]
struct HostState {
    uri: String,
    title: String,
    position_ms: i64,
    playing: bool,
    server_ms: i64,
    seq: u64,
}

/// What this app plays now, in the terms a Jam shares.
#[derive(Clone, Debug, PartialEq)]
struct Playing {
    uri: String,
    title: String,
    artist: String,
    duration_ms: u32,
    position_ms: u32,
    playing: bool,
}

pub struct Session {
    connection: Connection,
    pub code: String,
    pub host: bool,
    pub joined: bool,
    pub members: Vec<Member>,
    pub queue: Vec<QueueItem>,
    /// Server time minus this computer's, from the fastest ping.
    clock_offset: i64,
    best_rtt: i64,
    /// Host: what it last told the relay, and when.
    sent: Option<(Playing, Instant)>,
    /// Guest: the host's state, and the last one acted on.
    host_state: Option<HostState>,
    applied_seq: u64,
    settle_until: Option<Instant>,
    /// Guest: a local song of the host's this computer does not have.
    missing_told: Option<String>,
}

#[derive(Default)]
pub struct JamState {
    /// The Jam window is open.
    pub open: bool,
    pub name: String,
    pub code_input: String,
    pub session: Option<Session>,
    pub error: Option<String>,
}

impl JamState {
    pub fn in_jam(&self) -> bool {
        self.session.as_ref().is_some_and(|session| session.joined)
    }
}

impl App {
    /// The name others see, from the Spotify profile unless one was typed.
    pub fn jam_default_name(&self) -> String {
        self.user
            .as_ref()
            .and_then(|user| user.display_name.clone())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "Listener".into())
    }

    fn jam_connect(&mut self, code: String, host: bool, ctx: &egui::Context) {
        let Some(relay) = crate::jam::RELAY else {
            self.local_library.jam.error = Some("Jam isn't set up in this build yet.".into());
            return;
        };
        if !matches!(self.target(), Target::Local) {
            self.local_library.jam.error =
                Some("Jam plays on this computer. Turn on playback here in Settings.".into());
            return;
        }
        let name = if self.local_library.jam.name.trim().is_empty() {
            self.jam_default_name()
        } else {
            self.local_library.jam.name.trim().to_string()
        };
        let connection = Connection::open(relay, &code, ctx.clone());
        connection.send(&json!({
            "type": "hello",
            "name": name,
            "id": format!("{:016x}", rand::random::<u64>()),
            "host": host,
        }));
        self.local_library.jam.error = None;
        self.local_library.jam.session = Some(Session {
            connection,
            code,
            host,
            joined: false,
            members: Vec::new(),
            queue: Vec::new(),
            clock_offset: 0,
            best_rtt: i64::MAX,
            sent: None,
            host_state: None,
            applied_seq: 0,
            settle_until: None,
            missing_told: None,
        });
    }

    pub fn jam_start(&mut self, ctx: &egui::Context) {
        self.jam_connect(new_code(), true, ctx);
    }

    pub fn jam_join(&mut self, ctx: &egui::Context) {
        match clean_code(&self.local_library.jam.code_input) {
            Some(code) => self.jam_connect(code, false, ctx),
            None => {
                self.local_library.jam.error = Some("A Jam code has 6 letters and numbers.".into())
            }
        }
    }

    pub fn jam_leave(&mut self) {
        if self.local_library.jam.session.take().is_some() {
            self.toast("You left the Jam");
        }
    }

    /// What this computer plays, from whichever player has the bar.
    fn jam_playing(&self) -> Option<Playing> {
        if self.local_library.active {
            let track = self.local_library.shown_track()?;
            return Some(Playing {
                uri: crate::extras::local_uri(
                    &track.artist,
                    &track.album,
                    &track.title,
                    track.duration_ms,
                ),
                title: track.title.clone(),
                artist: track.artist.clone(),
                duration_ms: track.duration_ms,
                position_ms: self.local_library.position_ms,
                playing: self.local_library.shown_playing(),
            });
        }
        let now = self.now_playing()?;
        Some(Playing {
            uri: now.uri,
            title: now.title,
            artist: now
                .artists
                .iter()
                .map(|artist| artist.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            duration_ms: now.duration_ms,
            position_ms: now.position_ms,
            playing: now.playing,
        })
    }

    /// Runs once a frame: reads the relay, and keeps host and guests in step.
    pub(crate) fn jam_frame(&mut self, ctx: &egui::Context) {
        let mut messages = Vec::new();
        let mut closed = None;
        if let Some(session) = &self.local_library.jam.session {
            while let Some(incoming) = session.connection.try_recv() {
                match incoming {
                    Incoming::Message(message) => messages.push(message),
                    Incoming::Closed(reason) => closed = Some(reason),
                }
            }
        } else {
            return;
        }
        for message in messages {
            self.jam_message(message, ctx);
        }
        if let Some(reason) = closed {
            let was_joined = self.local_library.jam.in_jam();
            self.local_library.jam.session = None;
            if !reason.is_empty() {
                if was_joined {
                    self.toast(reason.clone());
                }
                self.local_library.jam.error = Some(reason);
            }
            return;
        }
        if !self.local_library.jam.in_jam() {
            return;
        }
        if self
            .local_library
            .jam
            .session
            .as_ref()
            .is_some_and(|session| session.host)
        {
            self.jam_host_frame();
        } else {
            self.jam_guest_frame(ctx);
        }
        theme::repaint_after(ctx, Duration::from_millis(500));
    }

    fn jam_message(&mut self, message: Value, ctx: &egui::Context) {
        let kind = message["type"].as_str().unwrap_or_default().to_string();
        let local_now = now_ms();
        match kind.as_str() {
            "welcome" => {
                let Some(session) = self.local_library.jam.session.as_mut() else {
                    return;
                };
                session.joined = true;
                if let Some(server) = message["server_ms"].as_i64() {
                    session.clock_offset = server - local_now;
                }
                session.members = members(&message["members"]);
                session.queue = queue(&message["queue"]);
                if let Some(state) = host_state(&message["state"]) {
                    session.host_state = Some(state);
                }
                let host = session.host;
                let code = crate::jam::show_code(&session.code);
                self.toast(if host {
                    format!("Jam started. Share the code {code}")
                } else {
                    "You joined the Jam".to_string()
                });
            }
            "error" => {
                let reason = message["message"]
                    .as_str()
                    .unwrap_or("The Jam service refused.")
                    .to_string();
                // A code that happens to be taken: try another.
                let retry = message["code"].as_str() == Some("taken")
                    && self
                        .local_library
                        .jam
                        .session
                        .as_ref()
                        .is_some_and(|s| s.host);
                self.local_library.jam.session = None;
                if retry {
                    self.jam_start(ctx);
                } else {
                    self.local_library.jam.error = Some(reason);
                }
            }
            "pong" => {
                let Some(session) = self.local_library.jam.session.as_mut() else {
                    return;
                };
                if let (Some(sent), Some(server)) =
                    (message["t"].as_i64(), message["server_ms"].as_i64())
                {
                    let rtt = (local_now - sent).max(0);
                    if rtt <= session.best_rtt {
                        session.best_rtt = rtt;
                        session.clock_offset = server + rtt / 2 - local_now;
                    }
                }
            }
            "members" => {
                if let Some(session) = self.local_library.jam.session.as_mut() {
                    let before = session.members.len();
                    session.members = members(&message["members"]);
                    let after = session.members.len();
                    if after > before && session.host {
                        // A newcomer needs the state now, not at the heartbeat.
                        session.sent = None;
                    }
                }
            }
            "state" => {
                if let Some(state) = host_state(&message)
                    && let Some(session) = self.local_library.jam.session.as_mut()
                {
                    session.host_state = Some(state);
                }
            }
            "queue" => {
                if let Some(session) = self.local_library.jam.session.as_mut() {
                    session.queue = queue(&message["items"]);
                }
            }
            "queued" => {
                let item = &message["item"];
                let is_host = self
                    .local_library
                    .jam
                    .session
                    .as_ref()
                    .is_some_and(|s| s.host);
                if let Some(uri) = item["uri"].as_str() {
                    let title = item["title"].as_str().unwrap_or_default();
                    let by = item["by"].as_str().unwrap_or("Someone");
                    if is_host {
                        // The host's player plays the Jam's queue.
                        self.add_to_queue(uri.to_string(), title.to_string());
                    }
                    self.toast(format!("{by} added {title}"));
                }
            }
            "command" => {
                if self
                    .local_library
                    .jam
                    .session
                    .as_ref()
                    .is_some_and(|s| s.host)
                {
                    self.jam_apply_command(&message["action"]);
                }
            }
            "ended" => {
                let reason = message["message"].as_str().unwrap_or("The Jam ended.");
                self.toast(reason.to_string());
                self.local_library.jam.session = None;
            }
            _ => {}
        }
    }

    /// Host: tells the relay what plays when it changes, and now and then.
    fn jam_host_frame(&mut self) {
        let playing = self.jam_playing();
        let Some(session) = self.local_library.jam.session.as_mut() else {
            return;
        };
        let Some(playing) = playing else {
            return;
        };
        let changed = match &session.sent {
            None => true,
            Some((sent, at)) => {
                let expected = i64::from(sent.position_ms)
                    + if sent.playing {
                        at.elapsed().as_millis() as i64
                    } else {
                        0
                    };
                sent.uri != playing.uri
                    || sent.playing != playing.playing
                    || (i64::from(playing.position_ms) - expected).abs() > DRIFT_MS
                    || at.elapsed() > HEARTBEAT
            }
        };
        if !changed {
            return;
        }
        session.connection.send(&json!({
            "type": "state",
            "track": {
                "uri": playing.uri,
                "title": playing.title,
                "artist": playing.artist,
                "duration_ms": playing.duration_ms,
            },
            "position_ms": playing.position_ms,
            "playing": playing.playing,
        }));
        // A Jam song from the shared list has started: it leaves the list.
        if let Some(item) = session.queue.iter().find(|item| item.uri == playing.uri) {
            session
                .connection
                .send(&json!({"type": "queue_done", "id": item.id}));
        }
        session.sent = Some((playing, Instant::now()));
    }

    /// Guest: plays what the host plays, where the host is.
    fn jam_guest_frame(&mut self, ctx: &egui::Context) {
        let mine = self.jam_playing();
        let Some(session) = self.local_library.jam.session.as_mut() else {
            return;
        };
        let Some(state) = session.host_state.clone() else {
            return;
        };
        if state.uri.is_empty() {
            return;
        }
        let fresh = state.seq > session.applied_seq;
        let settling = session
            .settle_until
            .is_some_and(|until| Instant::now() < until);
        if settling && !fresh {
            return;
        }
        let server_now = now_ms() + session.clock_offset;
        let target = if state.playing {
            state.position_ms + (server_now - state.server_ms).max(0)
        } else {
            state.position_ms
        };
        let target_ms = target.clamp(0, i64::from(u32::MAX)) as u32;
        session.applied_seq = state.seq;
        let same_song = mine.as_ref().is_some_and(|mine| mine.uri == state.uri);
        if !same_song {
            session.settle_until = Some(Instant::now() + SETTLE);
            if state.uri.starts_with("spotify:local:") {
                match self.local_library.find_uri(&state.uri) {
                    Some(track) => {
                        self.play_local_rows(vec![track], 0, ctx);
                        if target_ms > 1_000 {
                            self.local_library.position_ms = target_ms;
                            self.local_send(ctx, LocalCommand::Seek(target_ms));
                        }
                        if !state.playing {
                            self.toggle_local_play(ctx);
                        }
                    }
                    None => {
                        // Not on this computer: wait quietly for the next one.
                        let told = self
                            .local_library
                            .jam
                            .session
                            .as_ref()
                            .and_then(|session| session.missing_told.clone());
                        if told.as_deref() != Some(state.uri.as_str()) {
                            if self.local_library.active && self.local_library.shown_playing() {
                                self.toggle_local_play(ctx);
                            } else if self.spotify_playing() {
                                self.toggle_play();
                            }
                            self.toast(format!(
                                "The host is playing {}, a local file you don't have",
                                state.title
                            ));
                            if let Some(session) = self.local_library.jam.session.as_mut() {
                                session.missing_told = Some(state.uri.clone());
                            }
                        }
                    }
                }
                return;
            }
            self.deactivate_local_files();
            let mut request = PlayRequest::tracks(vec![state.uri.clone()]).starting_at_index(0);
            // Loading takes a moment; start a little ahead to land in step.
            request.position_ms = if state.playing {
                target_ms.saturating_add(800)
            } else {
                target_ms
            };
            // A paused host's song starts and then pauses once it settles.
            self.play_request(request, false);
            return;
        }
        let Some(mine) = mine else {
            return;
        };
        if mine.playing != state.playing {
            session.settle_until = Some(Instant::now() + SETTLE);
            if self.local_library.active {
                self.toggle_local_play(ctx);
            } else {
                self.toggle_play();
            }
            return;
        }
        if state.playing && (i64::from(mine.position_ms) - target).abs() > DRIFT_MS {
            session.settle_until = Some(Instant::now() + SETTLE);
            if self.local_library.active {
                self.local_library.position_ms = target_ms;
                self.local_send(ctx, LocalCommand::Seek(target_ms));
            } else {
                self.seek(target_ms);
            }
        }
    }

    /// Host: a guest's control.
    fn jam_apply_command(&mut self, action: &Value) {
        let kind = action["kind"].as_str().unwrap_or_default();
        let uris = || -> Vec<String> {
            action["uris"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .filter_map(|uri| uri.as_str().map(str::to_string))
                        .take(500)
                        .collect()
                })
                .unwrap_or_default()
        };
        let next = match kind {
            "toggle" => Some(Action::TogglePlay),
            "next" => Some(Action::Next),
            "previous" => Some(Action::Previous),
            "seek" => action["ms"]
                .as_u64()
                .map(|ms| Action::Seek(ms.min(u64::from(u32::MAX)) as u32)),
            "play_uris" => {
                let uris = uris();
                (!uris.is_empty()).then(|| Action::PlayUris {
                    uris,
                    index: action["index"].as_u64().unwrap_or(0) as u32,
                })
            }
            "play_context" => action["uri"].as_str().map(|uri| Action::PlayContext {
                uri: uri.to_string(),
                offset_uri: action["offset_uri"].as_str().map(str::to_string),
                offset_index: None,
            }),
            "shuffle_play" => action["uri"]
                .as_str()
                .map(|uri| Action::ShufflePlay(uri.to_string())),
            _ => None,
        };
        if let Some(next) = next {
            self.actions.push(next);
        }
    }

    /// Guests' controls go to the host; anyone's added songs go to the
    /// Jam's shared list. Returns the action when the Jam leaves it alone.
    pub(crate) fn jam_take(&mut self, action: Action) -> Option<Action> {
        let Some(session) = self.local_library.jam.session.as_ref() else {
            return Some(action);
        };
        if !session.joined {
            return Some(action);
        }
        if let Action::AddToQueue { uri, label } = &action {
            session.connection.send(&json!({
                "type": "queue_add",
                "uri": uri,
                "title": label,
                "artist": "",
            }));
            return None;
        }
        if session.host {
            return Some(action);
        }
        let command = match &action {
            Action::TogglePlay => json!({"kind": "toggle"}),
            Action::Next => json!({"kind": "next"}),
            Action::Previous => json!({"kind": "previous"}),
            Action::Seek(ms) => json!({"kind": "seek", "ms": ms}),
            Action::PlayUris { uris, index } => {
                json!({"kind": "play_uris", "uris": uris, "index": index})
            }
            Action::PlayContext {
                uri, offset_uri, ..
            } => json!({"kind": "play_context", "uri": uri, "offset_uri": offset_uri}),
            Action::ShufflePlay(uri) => json!({"kind": "shuffle_play", "uri": uri}),
            Action::PlayFromRow {
                context,
                uri,
                index,
            } => match context {
                RowContext::Context { uri: context, .. } => {
                    json!({"kind": "play_context", "uri": context, "offset_uri": uri})
                }
                RowContext::View { uris, .. } | RowContext::Uris(uris) => {
                    json!({"kind": "play_uris", "uris": uris.to_vec(), "index": index})
                }
                RowContext::Queue => return None,
            },
            _ => return Some(action),
        };
        session
            .connection
            .send(&json!({"type": "command", "action": command}));
        None
    }
}

impl App {
    /// Guest: the local bar's controls go to the host too.
    pub(crate) fn jam_take_local(&mut self, action: &LocalAction) -> bool {
        let guest = self
            .local_library
            .jam
            .session
            .as_ref()
            .is_some_and(|session| session.joined && !session.host);
        if !guest {
            return false;
        }
        let action = match action {
            LocalAction::TogglePlay => Action::TogglePlay,
            LocalAction::Next => Action::Next,
            LocalAction::Previous => Action::Previous,
            LocalAction::Seek(ms) => Action::Seek(*ms),
            _ => return false,
        };
        self.jam_take(action).is_none()
    }
}

fn members(value: &Value) -> Vec<Member> {
    value
        .as_array()
        .map(|list| {
            list.iter()
                .map(|member| Member {
                    id: member["id"].as_str().unwrap_or_default().to_string(),
                    name: member["name"].as_str().unwrap_or("Listener").to_string(),
                    host: member["host"].as_bool().unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn queue(value: &Value) -> Vec<QueueItem> {
    value
        .as_array()
        .map(|list| {
            list.iter()
                .map(|item| QueueItem {
                    id: item["id"].as_str().unwrap_or_default().to_string(),
                    uri: item["uri"].as_str().unwrap_or_default().to_string(),
                    title: item["title"].as_str().unwrap_or_default().to_string(),
                    artist: item["artist"].as_str().unwrap_or_default().to_string(),
                    by: item["by"].as_str().unwrap_or_default().to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn host_state(value: &Value) -> Option<HostState> {
    let track = value.get("track")?;
    Some(HostState {
        uri: track["uri"].as_str()?.to_string(),
        title: track["title"].as_str().unwrap_or_default().to_string(),
        position_ms: value["position_ms"].as_i64().unwrap_or(0),
        playing: value["playing"].as_bool().unwrap_or(false),
        server_ms: value["server_ms"].as_i64().unwrap_or(0),
        seq: value["seq"].as_u64().unwrap_or(0),
    })
}

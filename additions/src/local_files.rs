//! Local files: music kept on this computer, played without Spotify.
//!
//! The library is a list of folders and the songs found in them, kept in
//! `local-files.json` in the config folder. A scan walks the folders on its
//! own thread and reads each file's tags with Symphonia, reusing what the
//! last scan read for files that have not changed.
//!
//! Playback has its own thread and its own audio output, apart from
//! librespot's: rodio decodes the file and converts it to the device's
//! format, and fastframe-audio plays it. The thread also owns the queue, so
//! the next song starts even while the window is minimised or hidden.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use rodio::Source;
use rodio::source::UniformSourceIterator;
use serde::{Deserialize, Serialize};

/// File extensions the player can decode.
pub const EXTENSIONS: &[&str] = &["mp3", "flac", "m4a", "mp4", "aac", "ogg", "oga", "wav"];

/// The library file's name in the config folder.
pub const LIBRARY_FILE: &str = "local-files.json";

/// Pressing Previous later than this into a song restarts it.
const RESTART_AFTER_MS: u32 = 3_000;

/// How deep a scan follows folders inside folders.
const MAX_DEPTH: usize = 24;

/// Cover art is scaled to fit this many pixels on its longer side.
const COVER_PIXELS: u32 = 256;

/// One song on disk, as the last scan read it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LocalTrack {
    pub path: String,
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub track: Option<u32>,
    #[serde(default)]
    pub disc: Option<u32>,
    #[serde(default)]
    pub duration_ms: u32,
    /// Size and modification time, so a rescan rereads only changed files.
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub modified: u64,
}

impl LocalTrack {
    /// The artist, or a placeholder when the file names none.
    pub fn artist_or_unknown(&self) -> &str {
        if self.artist.is_empty() {
            "Unknown artist"
        } else {
            &self.artist
        }
    }

    /// Lowercased text a filter matches against.
    pub fn haystack(&self) -> String {
        format!("{} {} {}", self.title, self.artist, self.album).to_lowercase()
    }
}

/// What `local-files.json` holds.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LibraryFile {
    #[serde(default)]
    pub folders: Vec<String>,
    #[serde(default)]
    pub tracks: Vec<LocalTrack>,
    #[serde(default = "default_volume")]
    pub volume: f32,
    #[serde(default)]
    pub shuffle: bool,
    #[serde(default)]
    pub repeat: Repeat,
}

impl Default for LibraryFile {
    fn default() -> Self {
        Self {
            folders: Vec::new(),
            tracks: Vec::new(),
            volume: default_volume(),
            shuffle: false,
            repeat: Repeat::Off,
        }
    }
}

fn default_volume() -> f32 {
    0.8
}

/// What happens when the queue runs out or a song ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

impl Repeat {
    pub fn next(self) -> Self {
        match self {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        }
    }
}

/// Reads the library. A missing or unreadable file is an empty library.
pub fn load_library(path: &Path) -> LibraryFile {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            log::warn!("local files: cannot read {}: {error}", path.display());
            LibraryFile::default()
        }),
        Err(_) => LibraryFile::default(),
    }
}

/// The newest save that reached the disk, so an older one finishing later
/// never overwrites it.
static SAVED: Mutex<u64> = Mutex::new(0);

/// Writes the library on its own thread, atomically: a temporary file is
/// renamed over the old one. `generation` orders the saves.
pub fn save_library(path: PathBuf, library: LibraryFile, generation: u64) {
    thread::spawn(move || {
        let mut saved = SAVED.lock().unwrap_or_else(PoisonError::into_inner);
        if generation < *saved {
            return;
        }
        let result = (|| -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let bytes = serde_json::to_vec(&library).map_err(std::io::Error::other)?;
            let temporary = path.with_extension("json.tmp");
            std::fs::write(&temporary, bytes)?;
            std::fs::rename(&temporary, &path)
        })();
        match result {
            Ok(()) => *saved = generation,
            Err(error) => log::warn!("local files: cannot save {}: {error}", path.display()),
        }
    });
}

// Scanning ------------------------------------------------------------------

/// A scan in progress, read by the interface each frame.
#[derive(Default)]
pub struct ScanProgress {
    /// Audio files found so far.
    pub found: AtomicU32,
    /// Files whose tags are read (or reused).
    pub read: AtomicU32,
    pub finished: AtomicBool,
    result: Mutex<Option<Vec<LocalTrack>>>,
}

impl ScanProgress {
    /// The songs, once the scan has finished. Taken once.
    pub fn take(&self) -> Option<Vec<LocalTrack>> {
        if !self.finished.load(Ordering::Acquire) {
            return None;
        }
        self.result
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

/// Walks `folders` on a new thread, reusing `previous` for files whose size
/// and modification time are unchanged.
pub fn start_scan(
    folders: Vec<String>,
    previous: Vec<LocalTrack>,
    ctx: egui::Context,
) -> Arc<ScanProgress> {
    let progress = Arc::new(ScanProgress::default());
    let report = Arc::clone(&progress);
    let spawned = thread::Builder::new()
        .name("local-files-scan".into())
        .spawn(move || {
            let tracks = scan(&folders, previous, &report, &ctx);
            *report.result.lock().unwrap_or_else(PoisonError::into_inner) = Some(tracks);
            report.finished.store(true, Ordering::Release);
            ctx.request_repaint();
        });
    if let Err(error) = spawned {
        log::warn!("local files: cannot start a scan: {error}");
        *progress
            .result
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Vec::new());
        progress.finished.store(true, Ordering::Release);
    }
    progress
}

fn scan(
    folders: &[String],
    previous: Vec<LocalTrack>,
    progress: &ScanProgress,
    ctx: &egui::Context,
) -> Vec<LocalTrack> {
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    for folder in folders {
        walk(Path::new(folder), 0, &mut files, &mut seen, progress);
    }
    let known: HashMap<String, LocalTrack> = previous
        .into_iter()
        .map(|track| (track.path.clone(), track))
        .collect();
    let mut tracks = Vec::with_capacity(files.len());
    for (index, (path, text)) in files.into_iter().enumerate() {
        let (size, modified) = file_stamp(&path);
        let track = match known.get(&text) {
            Some(old) if old.size == size && old.modified == modified => old.clone(),
            _ => read_tags(&path, text, size, modified),
        };
        tracks.push(track);
        progress.read.store(index as u32 + 1, Ordering::Relaxed);
        if index % 50 == 0 {
            ctx.request_repaint();
        }
    }
    sort_tracks(&mut tracks);
    tracks
}

/// Collects the audio files under `dir`, skipping hidden folders and
/// symbolic links to folders (which could loop).
fn walk(
    dir: &Path,
    depth: usize,
    files: &mut Vec<(PathBuf, String)>,
    seen: &mut HashSet<String>,
    progress: &ScanProgress,
) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        log::warn!("local files: cannot read the folder {}", dir.display());
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let hidden = entry.file_name().to_string_lossy().starts_with('.');
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if !hidden {
                walk(&path, depth + 1, files, seen, progress);
            }
            continue;
        }
        let is_file = kind.is_file()
            || (kind.is_symlink() && std::fs::metadata(&path).is_ok_and(|meta| meta.is_file()));
        if !is_file || hidden || !is_audio(&path) {
            continue;
        }
        // A path that is not valid Unicode cannot be saved in the library.
        let Some(text) = path.to_str().map(str::to_string) else {
            continue;
        };
        if seen.insert(text.clone()) {
            files.push((path, text));
            progress.found.fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            EXTENSIONS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
}

fn file_stamp(path: &Path) -> (u64, u64) {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let modified = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_secs());
            (meta.len(), modified)
        }
        Err(_) => (0, 0),
    }
}

/// Artist, then album, then disc and track number, then title.
pub fn sort_tracks(tracks: &mut [LocalTrack]) {
    tracks.sort_by_cached_key(|track| {
        (
            track.artist.to_lowercase(),
            track.album.to_lowercase(),
            track.disc.unwrap_or(0),
            track.track.unwrap_or(u32::MAX),
            track.title.to_lowercase(),
        )
    });
}

#[derive(Default)]
struct Found {
    title: Option<String>,
    artist: Option<String>,
    album_artist: Option<String>,
    album: Option<String>,
    track: Option<u32>,
    disc: Option<u32>,
}

/// Reads a file's title, artist, album, numbers and length. Whatever the
/// file does not say comes from its name and folder.
fn read_tags(path: &Path, text: String, size: u64, modified: u64) -> LocalTrack {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| text.clone());
    let folder = path
        .parent()
        .and_then(|parent| parent.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut track = LocalTrack {
        path: text,
        title: stem,
        artist: String::new(),
        album: folder,
        track: None,
        disc: None,
        duration_ms: 0,
        size,
        modified,
    };
    let Ok(file) = File::open(path) else {
        return track;
    };
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
        hint.with_extension(extension);
    }
    let probed = symphonia::default::get_probe().format(
        &hint,
        stream,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    );
    let mut probed = match probed {
        Ok(probed) => probed,
        Err(error) => {
            log::debug!("local files: no tags in {}: {error}", path.display());
            return track;
        }
    };
    let mut found = Found::default();
    if let Some(metadata) = probed.metadata.get()
        && let Some(revision) = metadata.current()
    {
        apply_tags(&mut found, revision.tags());
    }
    if let Some(revision) = probed.format.metadata().current() {
        apply_tags(&mut found, revision.tags());
    }
    if let Some(default) = probed.format.default_track() {
        let params = &default.codec_params;
        if let (Some(frames), Some(rate)) = (params.n_frames, params.sample_rate)
            && rate > 0
        {
            track.duration_ms = (frames.saturating_mul(1000) / u64::from(rate)) as u32;
        } else if let (Some(frames), Some(base)) = (params.n_frames, params.time_base) {
            let time = base.calc_time(frames);
            track.duration_ms = (time.seconds * 1000 + (time.frac * 1000.0) as u64) as u32;
        }
    }
    if let Some(title) = found.title {
        track.title = title;
    }
    if let Some(artist) = found.artist.or(found.album_artist) {
        track.artist = artist;
    }
    if let Some(album) = found.album {
        track.album = album;
    }
    track.track = found.track;
    track.disc = found.disc;
    track
}

fn apply_tags(found: &mut Found, tags: &[symphonia::core::meta::Tag]) {
    use symphonia::core::meta::StandardTagKey;
    for tag in tags {
        let Some(key) = tag.std_key else {
            continue;
        };
        let value = tag.value.to_string();
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key {
            StandardTagKey::TrackTitle => {
                found.title.get_or_insert_with(|| value.to_string());
            }
            StandardTagKey::Artist => {
                found.artist.get_or_insert_with(|| value.to_string());
            }
            StandardTagKey::AlbumArtist => {
                found.album_artist.get_or_insert_with(|| value.to_string());
            }
            StandardTagKey::Album => {
                found.album.get_or_insert_with(|| value.to_string());
            }
            StandardTagKey::TrackNumber => {
                if found.track.is_none() {
                    found.track = leading_number(value);
                }
            }
            StandardTagKey::DiscNumber => {
                if found.disc.is_none() {
                    found.disc = leading_number(value);
                }
            }
            _ => {}
        }
    }
}

/// `3` from "3", "03" or "3/12".
fn leading_number(text: &str) -> Option<u32> {
    let digits: String = text
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// A song's cover, scaled down, as RGBA pixels.
#[derive(Clone)]
pub struct Cover {
    pub size: [usize; 2],
    pub rgba: Vec<u8>,
}

/// The picture inside the file, else a cover image beside it.
pub fn read_cover(path: &Path) -> Option<Cover> {
    embedded_cover(path)
        .or_else(|| folder_cover(path))
        .and_then(|bytes| {
            let image = image::load_from_memory(&bytes).ok()?;
            let image = image.thumbnail(COVER_PIXELS, COVER_PIXELS).to_rgba8();
            let size = [image.width() as usize, image.height() as usize];
            Some(Cover {
                size,
                rgba: image.into_raw(),
            })
        })
}

fn embedded_cover(path: &Path) -> Option<Vec<u8>> {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::{MetadataOptions, StandardVisualKey, Visual};
    use symphonia::core::probe::Hint;

    fn pick(visuals: &[Visual]) -> Option<Vec<u8>> {
        visuals
            .iter()
            .find(|visual| visual.usage == Some(StandardVisualKey::FrontCover))
            .or_else(|| visuals.first())
            .map(|visual| visual.data.to_vec())
    }

    let file = File::open(path).ok()?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
        hint.with_extension(extension);
    }
    let mut probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .ok()?;
    if let Some(metadata) = probed.metadata.get()
        && let Some(revision) = metadata.current()
        && let Some(bytes) = pick(revision.visuals())
    {
        return Some(bytes);
    }
    let metadata = probed.format.metadata();
    let revision = metadata.current()?;
    pick(revision.visuals())
}

fn folder_cover(path: &Path) -> Option<Vec<u8>> {
    let folder = path.parent()?;
    let entries = std::fs::read_dir(folder).ok()?;
    let mut best: Option<(usize, PathBuf)> = None;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        let Some((stem, extension)) = name.rsplit_once('.') else {
            continue;
        };
        if !matches!(extension, "jpg" | "jpeg" | "png") {
            continue;
        }
        let rank = match stem {
            "cover" => 0,
            "folder" => 1,
            "front" => 2,
            "albumart" | "album" => 3,
            stem if stem.starts_with("albumart") => 4,
            _ => continue,
        };
        if best.as_ref().is_none_or(|(old, _)| rank < *old) {
            best = Some((rank, entry.path()));
        }
    }
    std::fs::read(best?.1).ok()
}

// Playback ------------------------------------------------------------------

/// A decoded file converted to the device's rate and channel count.
type Stream = UniformSourceIterator<rodio::Decoder<BufReader<File>>>;

/// A rate and channel count as one number, for an atomic.
fn pack(sample_rate: u32, channels: u16) -> u64 {
    (u64::from(sample_rate) << 16) | u64::from(channels)
}

fn unpack(format: u64) -> (u32, u16) {
    ((format >> 16) as u32, (format & 0xffff) as u16)
}

/// What the audio callback and the player thread share.
struct RenderShared {
    stream: Mutex<Option<Stream>>,
    /// The format the device runs at, from `configure`.
    device_format: AtomicU64,
    /// The format `stream` produces. The callback plays silence while the
    /// two differ, until the player thread rebuilds the stream.
    stream_format: AtomicU64,
    playing: AtomicBool,
    /// Linear gain, as `f32` bits.
    gain: AtomicU32,
    /// Frames played since `base_ms`.
    frames: AtomicU64,
    base_ms: AtomicU64,
    ended: AtomicBool,
}

impl RenderShared {
    fn position_ms(&self) -> u32 {
        let (rate, _) = unpack(self.stream_format.load(Ordering::Acquire));
        let played = if rate > 0 {
            self.frames.load(Ordering::Relaxed) * 1000 / u64::from(rate)
        } else {
            0
        };
        (self.base_ms.load(Ordering::Relaxed) + played).min(u64::from(u32::MAX)) as u32
    }

    fn lock_stream(&self) -> std::sync::MutexGuard<'_, Option<Stream>> {
        self.stream.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Fills the device from the current stream, fading in and out on Play and
/// Pause so neither clicks.
struct FileRender {
    shared: Arc<RenderShared>,
    format: (u32, u16),
    gain: f32,
    fade: f32,
}

impl fastframe_audio::Render for FileRender {
    fn configure(&mut self, sample_rate: u32, channels: u16) {
        self.format = (sample_rate, channels);
        self.shared
            .device_format
            .store(pack(sample_rate, channels), Ordering::Release);
    }

    fn render(&mut self, out: &mut [f32]) {
        let playing = self.shared.playing.load(Ordering::Relaxed);
        if !playing && self.fade <= 0.0 {
            out.fill(0.0);
            return;
        }
        // Never wait for the player thread: a seek or a song change holds
        // the stream for a moment, and that moment is silence.
        let Ok(mut guard) = self.shared.stream.try_lock() else {
            out.fill(0.0);
            return;
        };
        let same_format =
            self.shared.stream_format.load(Ordering::Acquire) == pack(self.format.0, self.format.1);
        let Some(stream) = guard.as_mut().filter(|_| same_format) else {
            out.fill(0.0);
            return;
        };
        let channels = usize::from(self.format.1.max(1));
        let target = f32::from_bits(self.shared.gain.load(Ordering::Relaxed));
        // About twelve milliseconds from silence to full level.
        let step = 1.0 / (self.format.0.max(1) as f32 * 0.012);
        let mut frames = 0u64;
        let mut ended = false;
        for frame in out.chunks_mut(channels) {
            if ended {
                frame.fill(0.0);
                continue;
            }
            self.fade = if playing {
                (self.fade + step).min(1.0)
            } else {
                (self.fade - step).max(0.0)
            };
            if !playing && self.fade <= 0.0 {
                frame.fill(0.0);
                continue;
            }
            self.gain += (target - self.gain) * 0.002;
            let gain = self.gain * self.fade;
            for sample in frame.iter_mut() {
                match stream.next() {
                    Some(value) => *sample = value * gain,
                    None => {
                        ended = true;
                        *sample = 0.0;
                    }
                }
            }
            if !ended {
                frames += 1;
            }
        }
        self.shared.frames.fetch_add(frames, Ordering::Relaxed);
        if ended {
            self.shared.ended.store(true, Ordering::Release);
        }
    }
}

/// Opens `path` for the device's format. Returns the stream and its length
/// when the decoder knows it.
fn open_stream(path: &str, format: (u32, u16)) -> Result<(Stream, Option<Duration>), String> {
    let file = File::open(path).map_err(|error| error.to_string())?;
    let len = file.metadata().map_err(|error| error.to_string())?.len();
    let mut builder = rodio::Decoder::builder()
        .with_data(BufReader::new(file))
        .with_byte_len(len)
        .with_seekable(true);
    if let Some(extension) = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        builder = builder.with_hint(&extension.to_ascii_lowercase());
    }
    let decoder = builder.build().map_err(|error| error.to_string())?;
    let duration = decoder.total_duration();
    let (rate, channels) = format;
    Ok((
        UniformSourceIterator::new(decoder, channels.max(1), rate.max(1)),
        duration,
    ))
}

fn open_output(
    shared: &Arc<RenderShared>,
) -> Result<fastframe_audio::Output<FileRender>, fastframe_audio::OpenError> {
    let options = fastframe_audio::OutputOptions {
        device: fastframe_audio::Device::Default,
        channels: 2,
        sample_rate: None,
        buffer: fastframe_audio::Buffer::FixedOnWindows(fastframe_audio::BufferSize::Duration(
            Duration::from_millis(u64::from(crate::sink::DEFAULT_BUFFER_MS)),
        )),
        follow_default: true,
        ..fastframe_audio::OutputOptions::default()
    };
    let render = FileRender {
        shared: Arc::clone(shared),
        format: (0, 0),
        gain: 0.0,
        fade: 0.0,
    };
    let output = fastframe_audio::Output::open(options, render)?;
    log::info!("local files: audio output {}", output.device_name());
    Ok(output)
}

/// What the interface asks of the player thread.
pub enum Command {
    /// Plays `queue[index]`, then the rest in order.
    Load {
        queue: Vec<LocalTrack>,
        index: usize,
        generation: u64,
    },
    /// Replaces the order without interrupting the song (shuffle changed).
    Reorder {
        queue: Vec<LocalTrack>,
        index: usize,
    },
    Pause,
    Resume,
    Seek(u32),
    Next,
    Previous,
    SetRepeat(Repeat),
    Stop,
}

/// What the player is doing, for the interface.
#[derive(Clone, Default)]
pub struct Status {
    /// The last `Load` the thread took.
    pub generation: u64,
    pub current: Option<LocalTrack>,
    pub index: usize,
    pub queue_len: usize,
    pub playing: bool,
    pub duration_ms: u32,
    /// Counts song starts, so the interface knows when to fetch the cover.
    pub track_serial: u64,
    /// The last problem, with a serial so each is shown once.
    pub error: Option<(u64, String)>,
}

/// The playback thread's handle. Dropping it stops the thread.
pub struct Player {
    commands: Sender<Command>,
    render: Arc<RenderShared>,
    status: Arc<Mutex<Status>>,
    cover: Arc<Mutex<Option<(u64, Option<Cover>)>>>,
}

impl Player {
    pub fn new(ctx: egui::Context) -> Self {
        let (commands, receiver) = mpsc::channel();
        let render = Arc::new(RenderShared {
            stream: Mutex::new(None),
            device_format: AtomicU64::new(0),
            stream_format: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            gain: AtomicU32::new(0.0f32.to_bits()),
            frames: AtomicU64::new(0),
            base_ms: AtomicU64::new(0),
            ended: AtomicBool::new(false),
        });
        let status = Arc::new(Mutex::new(Status::default()));
        let cover = Arc::new(Mutex::new(None));
        let (thread_render, thread_status, thread_cover) =
            (Arc::clone(&render), Arc::clone(&status), Arc::clone(&cover));
        // The engine is made on its own thread: the audio output it opens
        // there never has to move between threads.
        if let Err(error) = thread::Builder::new()
            .name("local-files-player".into())
            .spawn(move || {
                Engine {
                    receiver,
                    render: thread_render,
                    status: thread_status,
                    cover: thread_cover,
                    ctx,
                    output: None,
                    queue: Vec::new(),
                    index: 0,
                    repeat: Repeat::Off,
                    paused_at: None,
                    failures: 0,
                    error_serial: 0,
                }
                .run()
            })
        {
            log::error!("local files: cannot start the player: {error}");
        }
        Self {
            commands,
            render,
            status,
            cover,
        }
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    pub fn status(&self) -> Status {
        self.status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn position_ms(&self) -> u32 {
        self.render.position_ms()
    }

    /// `level` from 0 to 1, on a curve that sounds even.
    pub fn set_volume(&self, level: f32) {
        let level = level.clamp(0.0, 1.0);
        self.render
            .gain
            .store((level * level).to_bits(), Ordering::Relaxed);
    }

    /// The cover for song start `serial`, once it is read. `Some(None)`
    /// means the song has none.
    pub fn take_cover(&self, serial: u64) -> Option<Option<Cover>> {
        let mut slot = self.cover.lock().unwrap_or_else(PoisonError::into_inner);
        if slot
            .as_ref()
            .is_some_and(|(for_serial, _)| *for_serial == serial)
        {
            return slot.take().map(|(_, cover)| cover);
        }
        None
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
    }
}

struct Engine {
    receiver: Receiver<Command>,
    render: Arc<RenderShared>,
    status: Arc<Mutex<Status>>,
    cover: Arc<Mutex<Option<(u64, Option<Cover>)>>>,
    ctx: egui::Context,
    output: Option<fastframe_audio::Output<FileRender>>,
    queue: Vec<LocalTrack>,
    index: usize,
    repeat: Repeat,
    paused_at: Option<Instant>,
    /// Songs in a row that would not open, so a folder of broken files
    /// does not spin forever.
    failures: usize,
    error_serial: u64,
}

impl Engine {
    fn run(mut self) {
        loop {
            let timeout = if self.render.playing.load(Ordering::Relaxed) {
                Duration::from_millis(50)
            } else {
                Duration::from_millis(250)
            };
            match self.receiver.recv_timeout(timeout) {
                Ok(command) => self.handle(command),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.maintain();
        }
        self.stop();
    }

    fn update(&self, change: impl FnOnce(&mut Status)) {
        change(&mut self.status.lock().unwrap_or_else(PoisonError::into_inner));
        self.ctx.request_repaint();
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Load {
                queue,
                index,
                generation,
            } => {
                self.queue = queue;
                self.index = index.min(self.queue.len().saturating_sub(1));
                self.failures = 0;
                let queue_len = self.queue.len();
                self.update(|status| {
                    status.generation = generation;
                    status.queue_len = queue_len;
                });
                self.start_current();
            }
            Command::Reorder { queue, index } => {
                self.queue = queue;
                self.index = index.min(self.queue.len().saturating_sub(1));
                let (queue_len, index) = (self.queue.len(), self.index);
                self.update(|status| {
                    status.queue_len = queue_len;
                    status.index = index;
                });
            }
            Command::Pause => self.pause(),
            Command::Resume => self.resume(),
            Command::Seek(ms) => self.seek(ms),
            Command::Next => self.advance(true),
            Command::Previous => {
                if self.render.position_ms() > RESTART_AFTER_MS || self.index == 0 {
                    self.seek(0);
                    self.resume();
                } else {
                    self.index -= 1;
                    self.failures = 0;
                    self.start_current();
                }
            }
            Command::SetRepeat(repeat) => self.repeat = repeat,
            Command::Stop => self.stop(),
        }
    }

    /// Moves to the next song. `pressed` is the Next button, which leaves
    /// a song set to repeat; a song that ended by itself repeats.
    fn advance(&mut self, pressed: bool) {
        if !pressed && self.repeat == Repeat::One {
            self.start_current();
            return;
        }
        if self.index + 1 < self.queue.len() {
            self.index += 1;
            self.start_current();
        } else if self.repeat != Repeat::Off && !self.queue.is_empty() {
            self.index = 0;
            self.start_current();
        } else {
            // The end of the queue: stay on the last song, paused at its
            // start, as Spotify does.
            self.render.playing.store(false, Ordering::Relaxed);
            self.seek(0);
            self.paused_at = Some(Instant::now());
            self.update(|status| status.playing = false);
        }
    }

    fn start_current(&mut self) {
        let Some(track) = self.queue.get(self.index).cloned() else {
            self.stop();
            return;
        };
        if self.output.is_none() {
            match open_output(&self.render) {
                Ok(output) => self.output = Some(output),
                Err(error) => {
                    log::warn!("local files: {error}");
                    self.report(format!("Couldn't open the audio output: {error}"));
                    self.render.playing.store(false, Ordering::Relaxed);
                    self.update(|status| status.playing = false);
                    return;
                }
            }
        }
        if let Some(output) = self.output.as_mut() {
            output.resume();
            // A reopen after a long pause reports its format here.
            let _ = output.maintain();
        }
        let format = unpack(self.render.device_format.load(Ordering::Acquire));
        match open_stream(&track.path, format) {
            Ok((stream, duration)) => {
                {
                    let mut slot = self.render.lock_stream();
                    *slot = Some(stream);
                    self.render
                        .stream_format
                        .store(pack(format.0, format.1), Ordering::Release);
                    self.render.frames.store(0, Ordering::Relaxed);
                    self.render.base_ms.store(0, Ordering::Relaxed);
                    self.render.ended.store(false, Ordering::Release);
                    self.render.playing.store(true, Ordering::Relaxed);
                }
                self.paused_at = None;
                self.failures = 0;
                let duration_ms = duration
                    .map(|duration| duration.as_millis().min(u128::from(u32::MAX)) as u32)
                    .filter(|ms| *ms > 0)
                    .unwrap_or(track.duration_ms);
                let index = self.index;
                let mut serial = 0;
                self.update(|status| {
                    status.current = Some(track.clone());
                    status.index = index;
                    status.playing = true;
                    status.duration_ms = duration_ms;
                    status.track_serial += 1;
                    serial = status.track_serial;
                });
                self.load_cover(serial, &track.path);
            }
            Err(error) => {
                log::warn!("local files: cannot play {}: {error}", track.path);
                let name = Path::new(&track.path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| track.title.clone());
                self.report(format!("Couldn't play {name}"));
                self.failures += 1;
                if self.failures < self.queue.len() {
                    self.advance(true);
                } else {
                    self.stop();
                }
            }
        }
    }

    /// Reads the cover on its own thread, so a large picture never delays
    /// the music.
    fn load_cover(&self, serial: u64, path: &str) {
        let slot = Arc::clone(&self.cover);
        let ctx = self.ctx.clone();
        let path = PathBuf::from(path);
        let _ = thread::Builder::new()
            .name("local-files-cover".into())
            .spawn(move || {
                let cover = read_cover(&path);
                *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some((serial, cover));
                ctx.request_repaint();
            });
    }

    fn report(&mut self, message: String) {
        self.error_serial += 1;
        let serial = self.error_serial;
        self.update(|status| status.error = Some((serial, message)));
    }

    fn pause(&mut self) {
        self.render.playing.store(false, Ordering::Relaxed);
        self.paused_at = Some(Instant::now());
        self.update(|status| status.playing = false);
    }

    fn resume(&mut self) {
        if self.render.lock_stream().is_none() {
            // Nothing loaded (the queue ended or was stopped): start over.
            if !self.queue.is_empty() {
                self.start_current();
            }
            return;
        }
        if let Some(output) = self.output.as_mut() {
            output.resume();
        }
        self.render.playing.store(true, Ordering::Relaxed);
        self.paused_at = None;
        self.update(|status| status.playing = true);
    }

    fn seek(&mut self, ms: u32) {
        let mut slot = self.render.lock_stream();
        let Some(stream) = slot.as_mut() else {
            return;
        };
        match stream.try_seek(Duration::from_millis(u64::from(ms))) {
            Ok(()) => {
                self.render.base_ms.store(u64::from(ms), Ordering::Relaxed);
                self.render.frames.store(0, Ordering::Relaxed);
                self.render.ended.store(false, Ordering::Release);
            }
            Err(error) => log::warn!("local files: cannot seek: {error}"),
        }
        drop(slot);
        self.ctx.request_repaint();
    }

    fn stop(&mut self) {
        self.render.playing.store(false, Ordering::Relaxed);
        *self.render.lock_stream() = None;
        self.render.frames.store(0, Ordering::Relaxed);
        self.render.base_ms.store(0, Ordering::Relaxed);
        self.queue.clear();
        self.index = 0;
        self.paused_at = Some(Instant::now());
        self.update(|status| {
            status.current = None;
            status.playing = false;
            status.queue_len = 0;
            status.index = 0;
            status.duration_ms = 0;
        });
    }

    /// Keeps the device healthy, follows a change of output format, lets
    /// the device rest during a pause, and moves on when a song ends.
    fn maintain(&mut self) {
        let Some(output) = self.output.as_mut() else {
            return;
        };
        for error in output.take_errors() {
            log::warn!("local files: audio output: {error}");
        }
        if let fastframe_audio::Maintained::Failed(error) = output.maintain() {
            log::warn!("local files: audio output: {error}");
        }
        let paused_long = self
            .paused_at
            .is_some_and(|at| at.elapsed() > Duration::from_millis(400));
        if paused_long && !output.is_paused() {
            output.pause();
        }
        let device = self.render.device_format.load(Ordering::Acquire);
        let stream = self.render.stream_format.load(Ordering::Acquire);
        let loaded = self.render.lock_stream().is_some();
        if loaded && device != stream && unpack(device).0 > 0 {
            self.rebuild();
        }
        if self.render.ended.swap(false, Ordering::AcqRel) {
            self.advance(false);
        }
    }

    /// The device now runs at another rate or channel count: reopen the
    /// song in that format where it was.
    fn rebuild(&mut self) {
        let Some(track) = self.queue.get(self.index).cloned() else {
            return;
        };
        let position = self.render.position_ms();
        let format = unpack(self.render.device_format.load(Ordering::Acquire));
        match open_stream(&track.path, format) {
            Ok((mut stream, _)) => {
                if position > 0
                    && let Err(error) = stream.try_seek(Duration::from_millis(u64::from(position)))
                {
                    log::warn!("local files: cannot return to {position} ms: {error}");
                }
                let mut slot = self.render.lock_stream();
                *slot = Some(stream);
                self.render
                    .base_ms
                    .store(u64::from(position), Ordering::Relaxed);
                self.render.frames.store(0, Ordering::Relaxed);
                self.render
                    .stream_format
                    .store(pack(format.0, format.1), Ordering::Release);
            }
            Err(error) => log::warn!("local files: cannot reopen {}: {error}", track.path),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_numbers_read_their_leading_digits() {
        assert_eq!(leading_number("3"), Some(3));
        assert_eq!(leading_number("03/12"), Some(3));
        assert_eq!(leading_number(" 7 "), Some(7));
        assert_eq!(leading_number("A"), None);
    }

    #[test]
    fn formats_survive_packing() {
        assert_eq!(unpack(pack(48_000, 2)), (48_000, 2));
        assert_eq!(unpack(pack(44_100, 1)), (44_100, 1));
    }

    #[test]
    fn audio_files_are_recognised_by_extension() {
        assert!(is_audio(Path::new("song.MP3")));
        assert!(is_audio(Path::new("a/b/song.flac")));
        assert!(!is_audio(Path::new("cover.jpg")));
        assert!(!is_audio(Path::new("noextension")));
    }

    #[test]
    fn the_library_reads_files_from_older_versions() {
        let library: LibraryFile = serde_json::from_str(r#"{"folders":["C:\\Music"]}"#).unwrap();
        assert_eq!(library.folders, vec!["C:\\Music".to_string()]);
        assert!(library.tracks.is_empty());
        assert_eq!(library.repeat, Repeat::Off);
    }
}

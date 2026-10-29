//! Discord Rich Presence: friends see "Krak · Clipping Valorant · 3 shards captured today", the
//! Krak tile, and a "Get Krak" button. No SDK: the Discord client listens on a local named pipe
//! (`\\.\pipe\discord-ipc-N`, N = 0..9) where each frame is an opcode and a length (u32 LE)
//! followed by JSON; a handshake with our client id, then `SET_ACTIVITY` commands.
//!
//! One background thread owns the connection:
//! - while the setting is on, it connects (and retries quietly every 20 s while Discord is
//!   closed; a closed or restarted Discord shows up as a broken pipe), answers pings, and every
//!   5 s looks at what is in front and at the replay / recording state;
//! - it sends the activity only when it changed, and never twice within 15 s (Discord accepts
//!   5 updates per 20 s), clearing it at once when the setting goes off;
//! - pipe reads only happen once a whole frame is there (`PeekNamedPipe`), with a deadline for
//!   replies: nothing waits forever, and nothing runs on the main thread.
//!
//! The game stays the activity's game while you alt-tab: the desktop takes over only after two
//! minutes in front, another game after 10 s; chat apps, browsers and launchers count as the
//! desktop. The elapsed time counts from when the current game (or the desktop) came to the
//! front: "for 25:12" means 25 minutes in this game, not since Krak started with Windows.
//!
//! "Shards captured today" counts the library's clips created since local midnight. The folder
//! is counted once, then again only after `shard-saved` / `recording-saved` /
//! `library-changed` (once per update, however many events) and when the day changes; the count
//! reads directory entries only, no file is opened, so big libraries stay cheap.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Listener, Manager};

use crate::capture::{self, CaptureState};
use crate::i18n::{self, Labels};
use crate::library;
use crate::replay::{self, Replay};
use crate::settings::Settings;
use crate::state::{AppState, lock};
use crate::watcher::LIBRARY_CHANGED;

/// The "Krak" application on the Discord developer portal, and its art assets.
const CLIENT_ID: &str = "1565168468159103067";
const LARGE_IMAGE: &str = "logo";
const IMAGE_READY: &str = "ready";
const IMAGE_RECORDING: &str = "rec";
const DOWNLOAD_URL: &str = "https://github.com/KryoStudio/kryo-shard/releases/latest";
/// Emitted when a continuous recording is saved (payload: the shard).
const RECORDING_SAVED: &str = "recording-saved";

const PIPE_PREFIX: &str = r"\\.\pipe\discord-ipc-";
const PIPES: u32 = 10;

const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
const OP_CLOSE: u32 = 2;
const OP_PING: u32 = 3;
const OP_PONG: u32 = 4;
/// Larger frames are not Discord's (READY is a few kB).
const MAX_FRAME: u32 = 1 << 20;

/// Discord closed: try again this often.
const RETRY: Duration = Duration::from_secs(20);
/// What is in front and the capture state are looked at this often.
const LOOK: Duration = Duration::from_secs(5);
/// Never two updates closer than this (Discord's limit: 5 per 20 s).
const MIN_INTERVAL: Duration = Duration::from_secs(15);
/// Longest wait for Discord's answer to a command.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// How long `get_presence` waits for a connection attempt it asked for.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(2);
/// The desktop replaces the game after this long in front, another game after `SWITCH`.
const DESKTOP_GRACE: Duration = Duration::from_secs(120);
const SWITCH_GRACE: Duration = Duration::from_secs(10);
/// Discord's limits on activity texts (UTF-16 units) and button labels.
const MAX_TEXT: usize = 128;
const MAX_LABEL: usize = 32;

/// Apps that come to the front without being played (display names, lowercase, matched as a
/// whole or as the first words): shown as the desktop, never as "Clipping Discord".
const NOT_GAMES: [&str; 27] = [
    "discord",
    "google chrome",
    "chrome",
    "firefox",
    "mozilla firefox",
    "microsoft edge",
    "opera",
    "brave browser",
    "vivaldi",
    "steam",
    "epicgameslauncher",
    "epic games launcher",
    "battle.net",
    "ea app",
    "ubisoft connect",
    "riot client",
    "gog galaxy",
    "xbox",
    "spotify",
    "obs",
    "obs studio",
    "microsoft teams",
    "slack",
    "whatsapp",
    "telegram",
    "visual studio code",
    "vlc media player",
];

/// `get_presence`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PresenceInfo {
    /// The setting.
    pub enabled: bool,
    /// Discord is running and talking to us.
    pub connected: bool,
}

/// Managed state: the link to the presence thread.
pub struct Presence {
    shared: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    signal: Mutex<Signal>,
    /// Wakes the thread (settings, library events, `get_presence`).
    wake: Condvar,
    /// Signalled after each connection attempt.
    attempted: Condvar,
    attempts: AtomicU64,
    connected: AtomicBool,
}

/// Why the thread was woken.
#[derive(Debug, Default)]
struct Signal {
    woken: bool,
    /// The library changed: count today's shards again.
    recount: bool,
    /// Try to connect now, without waiting for the next retry.
    retry: bool,
}

impl Shared {
    fn notify(&self, f: impl FnOnce(&mut Signal)) {
        let mut signal = lock(&self.signal);
        signal.woken = true;
        f(&mut signal);
        self.wake.notify_all();
    }

    /// Waits for a wake-up (or `timeout`) and takes what it was about.
    fn wait(&self, timeout: Option<Duration>) -> Signal {
        let mut signal = lock(&self.signal);
        let deadline = timeout.map(|t| Instant::now() + t);
        while !signal.woken {
            signal = match deadline {
                Some(deadline) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    self.wake
                        .wait_timeout(signal, left)
                        .unwrap_or_else(|e| e.into_inner())
                        .0
                }
                None => self.wake.wait(signal).unwrap_or_else(|e| e.into_inner()),
            };
        }
        std::mem::take(&mut *signal)
    }

    fn attempt_done(&self) {
        let _signal = lock(&self.signal);
        self.attempts.fetch_add(1, Ordering::SeqCst);
        self.attempted.notify_all();
    }
}

/// Starts the presence thread (after `replay::init`: it reads the capture state).
pub fn init(app: &AppHandle) {
    let shared = Arc::new(Shared::default());
    app.manage(Presence {
        shared: shared.clone(),
    });
    for event in [replay::SHARD_SAVED, RECORDING_SAVED, LIBRARY_CHANGED] {
        let shared = shared.clone();
        app.listen_any(event, move |_| shared.notify(|s| s.recount = true));
    }
    let worker = Worker::new(app.clone(), shared);
    if let Err(err) = std::thread::Builder::new()
        .name("krak-presence".into())
        .spawn(move || worker.run())
    {
        eprintln!("krak: cannot start the Discord status thread: {err}");
    }
}

/// After a settings update: turns the status on or off, or rebuilds it (language, folder).
pub fn settings_changed(app: &AppHandle, old: &Settings, new: &Settings) {
    let folder = old.clips_dir != new.clips_dir;
    if (old.discord_presence != new.discord_presence || old.language != new.language || folder)
        && let Some(presence) = app.try_state::<Presence>()
    {
        presence.shared.notify(|s| s.recount |= folder);
    }
}

/// The setting, and whether Discord is there. While it is not connected, asks for an attempt
/// right away and waits for it (briefly), so an opening settings page sees the truth.
pub fn info(app: &AppHandle) -> PresenceInfo {
    let enabled = app.state::<AppState>().settings().discord_presence;
    let Some(presence) = app.try_state::<Presence>() else {
        return PresenceInfo {
            enabled,
            connected: false,
        };
    };
    let shared = &presence.shared;
    if enabled && !shared.connected.load(Ordering::SeqCst) {
        let before = shared.attempts.load(Ordering::SeqCst);
        shared.notify(|s| s.retry = true);
        let deadline = Instant::now() + ANSWER_TIMEOUT;
        let mut signal = lock(&shared.signal);
        while shared.attempts.load(Ordering::SeqCst) == before {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            signal = shared
                .attempted
                .wait_timeout(signal, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
    PresenceInfo {
        enabled,
        connected: shared.connected.load(Ordering::SeqCst),
    }
}

// ---- The thread ----

struct Worker {
    app: AppHandle,
    shared: Arc<Shared>,
    connection: Option<Connection>,
    next_attempt: Instant,
    /// The last connection error logged (each new one is logged once).
    last_error: Option<String>,
    limiter: Limiter,
    /// What Discord shows now (`None`: nothing of ours).
    shown: Option<Activity>,
    tracker: Tracker,
    today: Today,
    next_look: Instant,
}

impl Worker {
    fn new(app: AppHandle, shared: Arc<Shared>) -> Self {
        let now = Instant::now();
        Self {
            app,
            shared,
            connection: None,
            next_attempt: now,
            last_error: None,
            limiter: Limiter::default(),
            shown: None,
            tracker: Tracker::default(),
            today: Today::default(),
            next_look: now,
        }
    }

    fn run(mut self) {
        let mut signal = Signal::default();
        loop {
            let settings = self.app.state::<AppState>().settings();
            if signal.recount {
                self.today.dirty = true;
            }
            let timeout = if settings.discord_presence {
                self.step(&settings, signal.retry);
                Some(LOOK)
            } else {
                self.turn_off();
                None
            };
            signal = self.shared.wait(timeout);
        }
    }

    fn step(&mut self, settings: &Settings, retry: bool) {
        let now = Instant::now();
        if self.connection.is_none() && (retry || now >= self.next_attempt) {
            self.connect(now);
        }
        if let Some(connection) = self.connection.as_mut()
            && let Err(err) = connection.poll()
        {
            self.lost(&err);
        }
        if now >= self.next_look {
            let seen = capture::game::foreground_game().filter(|name| !is_app(name));
            self.tracker.observe(seen, now, unix_seconds());
            self.next_look = now + LOOK;
        }
        if self.connection.is_none() || !self.limiter.ready(now) {
            return;
        }
        let activity = self.activity(settings);
        if self.shown.as_ref() == Some(&activity) {
            return;
        }
        let Some(connection) = self.connection.as_mut() else {
            return;
        };
        self.limiter.sent(now);
        match connection.set_activity(std::process::id(), Some(&activity)) {
            Ok(reply) => {
                refused(&reply);
                // Shown, or refused: either way not sent again until it changes.
                self.shown = Some(activity);
            }
            Err(err) => self.lost(&err),
        }
    }

    fn connect(&mut self, now: Instant) {
        match Connection::open(PIPE_PREFIX, CLIENT_ID) {
            Ok(connection) => {
                eprintln!("krak: connected to Discord");
                self.connection = Some(connection);
                self.shown = None;
                self.last_error = None;
                self.shared.connected.store(true, Ordering::SeqCst);
            }
            Err(err) => {
                self.next_attempt = now + RETRY;
                // Discord not running is the usual case: quiet. Anything else once.
                let message = err.to_string();
                if err.kind() != io::ErrorKind::NotFound
                    && self.last_error.as_ref() != Some(&message)
                {
                    eprintln!("krak: cannot connect to Discord: {message}");
                }
                self.last_error = Some(message);
            }
        }
        self.shared.attempt_done();
    }

    /// Discord went away (closed, restarted, or stopped answering).
    fn lost(&mut self, err: &io::Error) {
        eprintln!("krak: Discord connection lost ({err})");
        self.connection = None;
        self.shown = None;
        self.next_attempt = Instant::now() + RETRY;
        self.shared.connected.store(false, Ordering::SeqCst);
    }

    /// Setting off: clears what Discord shows and disconnects; turning it on reconnects at once.
    fn turn_off(&mut self) {
        let now = Instant::now();
        if let Some(mut connection) = self.connection.take() {
            if self.shown.is_some() {
                self.limiter.sent(now);
                let _ = connection.set_activity(std::process::id(), None);
            }
            eprintln!("krak: Discord status off");
        }
        self.shown = None;
        self.next_attempt = now;
        self.shared.connected.store(false, Ordering::SeqCst);
    }

    fn activity(&mut self, settings: &Settings) -> Activity {
        let labels = i18n::labels(settings.language.as_deref());
        let capture = match self.app.try_state::<Replay>() {
            Some(_) => {
                let status = replay::status(&self.app);
                if status.recording.is_some() {
                    Capture::Recording
                } else if status.state == CaptureState::Ready {
                    Capture::Ready
                } else {
                    Capture::Off
                }
            }
            None => Capture::Off,
        };
        let today = self.today.count(Path::new(&settings.clips_dir));
        Activity::new(
            labels,
            self.tracker.current.as_deref(),
            capture,
            today,
            self.tracker.since,
        )
    }
}

/// Logs Discord's refusal of an activity (an `ERROR` event instead of the echo).
fn refused(reply: &Value) {
    if reply["evt"] == "ERROR" {
        eprintln!(
            "krak: Discord refused the status: {}",
            reply["data"]["message"].as_str().unwrap_or("?")
        );
    }
}

fn is_app(name: &str) -> bool {
    let name = name.to_lowercase();
    NOT_GAMES.iter().any(|app| {
        name == *app
            || name
                .strip_prefix(app)
                .is_some_and(|rest| rest.starts_with(' '))
    })
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// ---- What friends see ----

/// The capture side of the activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capture {
    /// A continuous recording is running.
    Recording,
    /// The replay buffer is running.
    Ready,
    /// Paused, starting or failed: no small image.
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Activity {
    details: String,
    state: String,
    /// Unix seconds: Discord shows the time elapsed since.
    start: u64,
    /// Small image and its hover text.
    small: Option<(&'static str, Option<String>)>,
    button: String,
}

impl Activity {
    fn new(labels: &Labels, game: Option<&str>, capture: Capture, today: u64, since: u64) -> Self {
        let details = match game {
            Some(game) => labels.presence_playing.replace("{game}", game),
            None => labels.presence_desktop.to_string(),
        };
        let small = match capture {
            Capture::Recording => Some((
                IMAGE_RECORDING,
                Some(fit(labels.presence_recording, MAX_TEXT)),
            )),
            Capture::Ready => Some((IMAGE_READY, None)),
            Capture::Off => None,
        };
        Self {
            details: fit(&details, MAX_TEXT),
            state: fit(&labels.shards_today(today), MAX_TEXT),
            start: since,
            small,
            button: fit(labels.presence_button, MAX_LABEL),
        }
    }

    fn to_json(&self) -> Value {
        let mut assets = json!({ "large_image": LARGE_IMAGE, "large_text": "Krak" });
        if let Some((image, text)) = &self.small {
            assets["small_image"] = json!(image);
            if let Some(text) = text {
                assets["small_text"] = json!(text);
            }
        }
        json!({
            "details": self.details,
            "state": self.state,
            "timestamps": { "start": self.start },
            "assets": assets,
            "buttons": [{ "label": self.button, "url": DOWNLOAD_URL }],
            "instance": false,
        })
    }
}

/// `text` within `max` UTF-16 units (Discord's measure), ending with "…" when cut.
fn fit(text: &str, max: usize) -> String {
    if text.encode_utf16().count() <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut units = 1; // the ellipsis
    for c in text.chars() {
        units += c.len_utf16();
        if units > max {
            break;
        }
        out.push(c);
    }
    out.truncate(out.trim_end().len());
    out.push('…');
    out
}

/// The `SET_ACTIVITY` command (`None` clears).
fn set_activity_command(pid: u32, activity: Option<&Activity>, nonce: &str) -> Value {
    let mut args = json!({ "pid": pid });
    if let Some(activity) = activity {
        args["activity"] = activity.to_json();
    }
    json!({ "cmd": "SET_ACTIVITY", "args": args, "nonce": nonce })
}

/// At most one update per [`MIN_INTERVAL`].
#[derive(Debug, Default)]
struct Limiter {
    last: Option<Instant>,
}

impl Limiter {
    fn ready(&self, now: Instant) -> bool {
        self.last
            .is_none_or(|last| now.saturating_duration_since(last) >= MIN_INTERVAL)
    }

    fn sent(&mut self, now: Instant) {
        self.last = Some(now);
    }
}

/// The game the activity shows, kept through short alt-tabs.
#[derive(Debug, Default)]
struct Tracker {
    started: bool,
    current: Option<String>,
    /// Unix seconds when `current` came to the front.
    since: u64,
    /// Something else in front: what, since when (monotonic and unix seconds).
    candidate: Option<(Option<String>, Instant, u64)>,
}

impl Tracker {
    fn observe(&mut self, seen: Option<String>, now: Instant, wall: u64) {
        if !std::mem::replace(&mut self.started, true) {
            (self.current, self.since) = (seen, wall);
            return;
        }
        if seen == self.current {
            self.candidate = None;
            return;
        }
        let grace = if seen.is_some() {
            SWITCH_GRACE
        } else {
            DESKTOP_GRACE
        };
        match &self.candidate {
            Some((candidate, at, at_wall)) if *candidate == seen => {
                if now.saturating_duration_since(*at) >= grace {
                    (self.current, self.since) = (seen, *at_wall);
                    self.candidate = None;
                }
            }
            _ => self.candidate = Some((seen, now, wall)),
        }
    }
}

/// Today's shard count, recounted when the library changed or the day did.
#[derive(Debug)]
struct Today {
    count: u64,
    day: (u16, u16, u16),
    dirty: bool,
}

impl Default for Today {
    fn default() -> Self {
        Self {
            count: 0,
            day: (0, 0, 0),
            dirty: true,
        }
    }
}

impl Today {
    fn count(&mut self, clips_dir: &Path) -> u64 {
        let (midnight, day) = local_midnight();
        if self.dirty || day != self.day {
            self.count = count_since(clips_dir, midnight);
            (self.day, self.dirty) = (day, false);
        }
        self.count
    }
}

/// Unix ms of the last local midnight, and today's local date. (On the two daylight-saving
/// days, midnight is off by the hour that changed.)
fn local_midnight() -> (u64, (u16, u16, u16)) {
    // SAFETY: no preconditions.
    let local = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let into_day =
        (u64::from(local.wHour) * 3600 + u64::from(local.wMinute) * 60 + u64::from(local.wSecond))
            * 1000
            + u64::from(local.wMilliseconds);
    (
        now.saturating_sub(into_day),
        (local.wYear, local.wMonth, local.wDay),
    )
}

/// Clips the library lists (the folder and its game folders, no links) created at or after
/// `since` (unix ms). Directory entries only: nothing is opened.
fn count_since(clips_dir: &Path, since: u64) -> u64 {
    let new_clip = |entry: &fs::DirEntry| {
        entry.file_type().is_ok_and(|t| t.is_file())
            && library::is_video(&entry.path())
            && entry
                .metadata()
                .is_ok_and(|meta| library::created_ms(&meta) >= since)
    };
    let mut count = 0;
    for entry in fs::read_dir(clips_dir).into_iter().flatten().flatten() {
        let folder = entry.file_type().is_ok_and(|t| t.is_dir())
            && !entry.file_name().to_string_lossy().starts_with(['.', '$']);
        if folder {
            let files = fs::read_dir(entry.path()).into_iter().flatten().flatten();
            count += files.filter(|file| new_clip(file)).count() as u64;
        } else if new_clip(&entry) {
            count += 1;
        }
    }
    count
}

// ---- The pipe ----

/// A frame: opcode, length (u32 little-endian), JSON.
fn encode(op: u32, payload: &Value) -> Vec<u8> {
    let body = payload.to_string();
    let mut frame = Vec::with_capacity(8 + body.len());
    frame.extend_from_slice(&op.to_le_bytes());
    frame.extend_from_slice(&u32::try_from(body.len()).unwrap_or(u32::MAX).to_le_bytes());
    frame.extend_from_slice(body.as_bytes());
    frame
}

/// Opcode and payload length of a frame header.
fn header(bytes: [u8; 8]) -> (u32, u32) {
    let [a, b, c, d, e, f, g, h] = bytes;
    (
        u32::from_le_bytes([a, b, c, d]),
        u32::from_le_bytes([e, f, g, h]),
    )
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

/// A connection to the Discord client.
struct Connection {
    pipe: File,
    commands: u64,
}

impl Connection {
    /// The first of `<prefix>0..9` that completes the handshake.
    fn open(prefix: &str, client_id: &str) -> io::Result<Self> {
        let mut last = io::Error::from(io::ErrorKind::NotFound);
        for n in 0..PIPES {
            let pipe = match OpenOptions::new()
                .read(true)
                .write(true)
                .open(format!("{prefix}{n}"))
            {
                Ok(pipe) => pipe,
                Err(err) => {
                    if err.kind() != io::ErrorKind::NotFound {
                        last = err;
                    }
                    continue;
                }
            };
            let mut connection = Self { pipe, commands: 0 };
            match connection.handshake(client_id) {
                Ok(()) => return Ok(connection),
                Err(err) => last = err,
            }
        }
        Err(last)
    }

    fn handshake(&mut self, client_id: &str) -> io::Result<()> {
        self.send(OP_HANDSHAKE, &json!({ "v": 1, "client_id": client_id }))?;
        let deadline = Instant::now() + REPLY_TIMEOUT;
        loop {
            match self.receive(deadline)? {
                (OP_FRAME, reply) if reply["evt"] == "READY" => return Ok(()),
                (OP_CLOSE, reply) => return Err(closed(&reply)),
                _ => {}
            }
        }
    }

    /// Sets (or clears) our activity; returns Discord's reply (the echo, or an `ERROR` event).
    fn set_activity(&mut self, pid: u32, activity: Option<&Activity>) -> io::Result<Value> {
        self.commands += 1;
        let nonce = format!("krak-{pid}-{}", self.commands);
        self.send(OP_FRAME, &set_activity_command(pid, activity, &nonce))?;
        let deadline = Instant::now() + REPLY_TIMEOUT;
        loop {
            match self.receive(deadline)? {
                (OP_CLOSE, reply) => return Err(closed(&reply)),
                (OP_FRAME, reply) if reply["nonce"] == nonce.as_str() => return Ok(reply),
                _ => {}
            }
        }
    }

    /// Reads whatever Discord sent (answering pings); an error once the pipe broke or Discord
    /// closed it.
    fn poll(&mut self) -> io::Result<()> {
        while let Some((op, reply)) = self.try_receive()? {
            if op == OP_CLOSE {
                return Err(closed(&reply));
            }
        }
        Ok(())
    }

    fn send(&mut self, op: u32, payload: &Value) -> io::Result<()> {
        self.pipe.write_all(&encode(op, payload))?;
        self.pipe.flush()
    }

    /// The next frame, waiting until `deadline`.
    fn receive(&mut self, deadline: Instant) -> io::Result<(u32, Value)> {
        loop {
            if let Some(frame) = self.try_receive()? {
                return Ok(frame);
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Discord did not answer",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A whole frame if one has arrived (never blocks). Pings are answered on the way.
    fn try_receive(&mut self) -> io::Result<Option<(u32, Value)>> {
        loop {
            let mut head = [0u8; 8];
            let available = peek(&self.pipe, &mut head)?;
            if available < head.len() {
                return Ok(None);
            }
            let (op, len) = header(head);
            if len > MAX_FRAME {
                return Err(invalid("oversized frame"));
            }
            let total = head.len() + len as usize;
            if available < total {
                return Ok(None);
            }
            let mut frame = vec![0u8; total];
            self.pipe.read_exact(&mut frame)?;
            let payload: Value =
                serde_json::from_slice(&frame[head.len()..]).map_err(|_| invalid("not JSON"))?;
            if op == OP_PING {
                self.send(OP_PONG, &payload)?;
                continue;
            }
            return Ok(Some((op, payload)));
        }
    }
}

fn closed(reply: &Value) -> io::Error {
    let message = reply["message"].as_str().unwrap_or("closed");
    io::Error::new(
        io::ErrorKind::ConnectionAborted,
        format!("Discord closed the connection: {message}"),
    )
}

/// Bytes waiting in the pipe, copying the first ones into `head` (nothing is consumed).
fn peek(pipe: &File, head: &mut [u8]) -> io::Result<usize> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Pipes::PeekNamedPipe;
    let (mut read, mut available) = (0u32, 0u32);
    // SAFETY: a pipe handle we own; the buffer is `head`, of the size given.
    unsafe {
        PeekNamedPipe(
            HANDLE(pipe.as_raw_handle()),
            Some(head.as_mut_ptr().cast()),
            u32::try_from(head.len()).unwrap_or(0),
            Some(&mut read),
            Some(&mut available),
            None,
        )
    }
    .map_err(|err| io::Error::new(io::ErrorKind::BrokenPipe, err))?;
    Ok(available as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn en() -> &'static Labels {
        i18n::labels(Some("en"))
    }

    #[test]
    fn frames_round_trip() {
        let payload = json!({ "v": 1, "client_id": CLIENT_ID });
        let frame = encode(OP_HANDSHAKE, &payload);
        let (op, len) = header(frame[..8].try_into().unwrap());
        assert_eq!((op, len as usize), (OP_HANDSHAKE, frame.len() - 8));
        assert_eq!(&frame[..4], &[0, 0, 0, 0]);
        let body: Value = serde_json::from_slice(&frame[8..]).unwrap();
        assert_eq!(body, payload);
        let frame = encode(OP_PONG, &json!({}));
        assert_eq!(frame, [4, 0, 0, 0, 2, 0, 0, 0, b'{', b'}']);
    }

    #[test]
    fn builds_the_activity() {
        let a = Activity::new(en(), Some("Valorant"), Capture::Ready, 3, 1_700_000_000);
        assert_eq!(
            set_activity_command(42, Some(&a), "n1"),
            json!({
                "cmd": "SET_ACTIVITY",
                "nonce": "n1",
                "args": {
                    "pid": 42,
                    "activity": {
                        "details": "Clipping Valorant",
                        "state": "3 shards captured today",
                        "timestamps": { "start": 1_700_000_000 },
                        "assets": {
                            "large_image": "logo", "large_text": "Krak", "small_image": "ready"
                        },
                        "buttons": [{
                            "label": "Get Krak",
                            "url": "https://github.com/KryoStudio/kryo-shard/releases/latest"
                        }],
                        "instance": false
                    }
                }
            })
        );
        // Recording: the red dot with its text; the desktop; one shard.
        let a = Activity::new(en(), None, Capture::Recording, 1, 5);
        let json = a.to_json();
        assert_eq!(json["details"], "On the desktop");
        assert_eq!(json["state"], "1 shard captured today");
        assert_eq!(json["assets"]["small_image"], "rec");
        assert_eq!(json["assets"]["small_text"], "Recording their game");
        // Paused: no small image.
        let a = Activity::new(en(), None, Capture::Off, 0, 5).to_json();
        assert!(a["assets"].get("small_image").is_none());
        // Other languages, with their plurals.
        let fr = Activity::new(
            i18n::labels(Some("fr")),
            Some("Valorant"),
            Capture::Ready,
            0,
            5,
        );
        assert_eq!(fr.details, "Clippe sur Valorant");
        assert_eq!(fr.state, i18n::labels(Some("fr")).shards_today(0));
        // Clearing: no activity at all.
        let clear = set_activity_command(42, None, "n2");
        assert!(clear["args"].get("activity").is_none());
        assert_eq!(clear["args"]["pid"], 42);
    }

    #[test]
    fn fits_discord_limits() {
        assert_eq!(fit("Get Krak", MAX_LABEL), "Get Krak");
        let long = "a".repeat(200);
        let cut = fit(&format!("Clipping {long}"), MAX_TEXT);
        assert_eq!(cut.encode_utf16().count(), MAX_TEXT);
        assert!(cut.ends_with('…'));
        // Counted in UTF-16 units, never splitting a character.
        let emoji = "🎮".repeat(100);
        let cut = fit(&emoji, MAX_TEXT);
        assert!(cut.encode_utf16().count() <= MAX_TEXT);
        assert!(cut.trim_end_matches('…').chars().all(|c| c == '🎮'));
        let a = Activity::new(en(), Some(&long), Capture::Ready, 2, 0);
        assert!(a.details.encode_utf16().count() <= MAX_TEXT);
    }

    #[test]
    fn spaces_updates() {
        let start = Instant::now();
        let mut limiter = Limiter::default();
        assert!(limiter.ready(start), "the first update goes at once");
        limiter.sent(start);
        assert!(!limiter.ready(start + Duration::from_secs(14)));
        assert!(limiter.ready(start + MIN_INTERVAL));
        // However sends are spread, never more than Discord's 5 in 20 s.
        let mut sent = Vec::new();
        let mut limiter = Limiter::default();
        for ms in (0..120_000).step_by(250) {
            let now = start + Duration::from_millis(ms);
            if limiter.ready(now) {
                limiter.sent(now);
                sent.push(ms);
            }
        }
        for (i, &at) in sent.iter().enumerate() {
            let in_window = sent[i..].iter().take_while(|&&t| t < at + 20_000).count();
            assert!(in_window <= 2, "{in_window} updates within 20 s of {at} ms");
        }
    }

    #[test]
    fn keeps_the_game_through_alt_tabs() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let game = |name: &str| Some(name.to_string());
        let mut tracker = Tracker::default();
        tracker.observe(game("Valorant"), at(0), 1000);
        assert_eq!(
            (tracker.current.as_deref(), tracker.since),
            (Some("Valorant"), 1000)
        );
        // A minute in Discord (the desktop): still Valorant.
        for s in (5..65).step_by(5) {
            tracker.observe(None, at(s), 1000 + s);
        }
        tracker.observe(game("Valorant"), at(65), 1065);
        assert_eq!(
            (tracker.current.as_deref(), tracker.since),
            (Some("Valorant"), 1000)
        );
        // Two minutes on the desktop: the desktop, since it came to the front.
        for s in (70..=190).step_by(5) {
            tracker.observe(None, at(s), 1000 + s);
        }
        assert_eq!((tracker.current.as_deref(), tracker.since), (None, 1070));
        // Another game: after 10 s, timed from when it came to the front.
        tracker.observe(game("Fortnite"), at(200), 1200);
        assert_eq!(tracker.current, None);
        tracker.observe(game("Fortnite"), at(205), 1205);
        tracker.observe(game("Fortnite"), at(210), 1210);
        assert_eq!(
            (tracker.current.as_deref(), tracker.since),
            (Some("Fortnite"), 1200)
        );
        // A flicker to another game does not count.
        tracker.observe(game("Valorant"), at(215), 1215);
        tracker.observe(game("Fortnite"), at(220), 1220);
        tracker.observe(game("Valorant"), at(225), 1225);
        assert_eq!(tracker.current.as_deref(), Some("Fortnite"));
    }

    #[test]
    fn apps_are_not_games() {
        for app in [
            "Discord",
            "Discord PTB",
            "Google Chrome",
            "Firefox",
            "Microsoft Edge",
            "Opera GX",
            "Steam",
            "Steam Client WebHelper",
            "OBS Studio",
            "Spotify",
            "Battle.net",
        ] {
            assert!(is_app(app), "{app}");
        }
        for game in [
            "Valorant",
            "SteamWorld Dig 2",
            "Obsidian",
            "EA Sports FC 25",
            "Rocket League",
        ] {
            assert!(!is_app(game), "{game}");
        }
    }

    #[test]
    fn counts_todays_shards() {
        use std::fs::FileTimes;
        use std::os::windows::fs::FileTimesExt;
        let dir = std::env::temp_dir().join(format!("kryo-presence-today-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("Valorant")).unwrap();
        fs::create_dir_all(dir.join(".trash")).unwrap();
        let yesterday = SystemTime::now() - Duration::from_secs(48 * 3600);
        let clip = |rel: &str, old: bool| {
            let path = dir.join(rel);
            let file = File::create(&path).unwrap();
            if old {
                let times = FileTimes::new()
                    .set_created(yesterday)
                    .set_modified(yesterday);
                file.set_times(times).unwrap();
            }
        };
        clip("Desktop 1.mp4", false);
        clip(r"Valorant\Ace.mp4", false);
        clip(r"Valorant\Clutch.MKV", false);
        clip(r"Valorant\Old.mp4", true);
        clip("notes.txt", false);
        clip(r".trash\Hidden.mp4", false);
        let (midnight, _) = local_midnight();
        // Files made just now are after midnight (unless the test runs across it).
        assert_eq!(
            count_since(&dir, midnight.min(unix_seconds() * 1000 - 60_000)),
            3
        );
        assert_eq!(count_since(&dir, 0), 4);
        assert_eq!(count_since(&dir.join("missing"), 0), 0);
        let mut today = Today::default();
        assert_eq!(today.count(&dir), count_since(&dir, midnight));
        clip("Desktop 2.mp4", false);
        assert_eq!(today.count(&dir), count_since(&dir, midnight) - 1, "cached");
        today.dirty = true;
        assert_eq!(today.count(&dir), count_since(&dir, midnight), "recounted");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_midnight_is_today() {
        let (midnight, (year, month, day)) = local_midnight();
        let now = unix_seconds() * 1000;
        assert!(midnight <= now + 1000 && now - midnight < 25 * 3600 * 1000);
        assert!(year >= 2024 && (1..=12).contains(&month) && (1..=31).contains(&day));
    }

    // ---- A Discord stand-in on a pipe of our own ----

    /// The server end of a named pipe `name`.
    fn serve(name: &str) -> File {
        use std::os::windows::io::FromRawHandle;
        use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
        use windows::Win32::System::Pipes::{
            CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
        };
        use windows::core::HSTRING;
        // SAFETY: a new pipe instance; the handle is moved into the File that closes it.
        unsafe {
            let handle = CreateNamedPipeW(
                &HSTRING::from(name),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1,
                65_536,
                65_536,
                0,
                None,
            );
            assert!(!handle.is_invalid(), "pipe {name}");
            File::from_raw_handle(handle.0)
        }
    }

    /// Reads one frame on the server side (waiting for the client to connect).
    fn read_frame(server: &mut File) -> (u32, Value) {
        let mut head = [0u8; 8];
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match server.read_exact(&mut head) {
                Ok(()) => break,
                // ERROR_PIPE_LISTENING: no client yet.
                Err(err) if err.raw_os_error() == Some(536) && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("server read: {err}"),
            }
        }
        let (op, len) = header(head);
        let mut body = vec![0u8; len as usize];
        server.read_exact(&mut body).unwrap();
        (op, serde_json::from_slice(&body).unwrap())
    }

    fn write_frame(server: &mut File, op: u32, payload: Value) {
        server.write_all(&encode(op, &payload)).unwrap();
    }

    #[test]
    fn talks_to_a_discord_like_pipe() {
        let prefix = format!(r"\\.\pipe\krak-test-ipc-{}-", std::process::id());
        // Only pipe 1 exists: pipe 0 is skipped.
        let mut server = serve(&format!("{prefix}1"));
        // The server end stays open until the client has read everything.
        let (done, finished) = std::sync::mpsc::channel::<()>();
        let discord = std::thread::spawn(move || {
            let (op, hello) = read_frame(&mut server);
            assert_eq!(op, OP_HANDSHAKE);
            assert_eq!(hello, json!({ "v": 1, "client_id": "123" }));
            write_frame(
                &mut server,
                OP_FRAME,
                json!({ "cmd": "DISPATCH", "evt": "READY", "data": { "v": 1 } }),
            );
            let (op, set) = read_frame(&mut server);
            assert_eq!(op, OP_FRAME);
            assert_eq!(set["cmd"], "SET_ACTIVITY");
            assert_eq!(set["args"]["activity"]["details"], "Clipping Valorant");
            // A ping, then a reply to someone else, then ours.
            write_frame(&mut server, OP_PING, json!({ "n": 7 }));
            write_frame(
                &mut server,
                OP_FRAME,
                json!({ "cmd": "SET_ACTIVITY", "nonce": "x" }),
            );
            write_frame(
                &mut server,
                OP_FRAME,
                json!({ "cmd": "SET_ACTIVITY", "nonce": set["nonce"], "data": set["args"]["activity"] }),
            );
            let (op, pong) = read_frame(&mut server);
            assert_eq!((op, pong), (OP_PONG, json!({ "n": 7 })));
            // Clearing; Discord refuses it (an ERROR event is still an answer).
            let (_, clear) = read_frame(&mut server);
            assert!(clear["args"].get("activity").is_none());
            write_frame(
                &mut server,
                OP_FRAME,
                json!({ "cmd": "SET_ACTIVITY", "evt": "ERROR", "nonce": clear["nonce"],
                        "data": { "code": 4000, "message": "nope" } }),
            );
            // Discord quits.
            write_frame(
                &mut server,
                OP_CLOSE,
                json!({ "code": 1000, "message": "bye" }),
            );
            let _ = finished.recv_timeout(Duration::from_secs(5));
        });

        let mut connection = Connection::open(&prefix, "123").expect("handshake");
        let activity = Activity::new(en(), Some("Valorant"), Capture::Ready, 2, 9);
        let reply = connection.set_activity(42, Some(&activity)).expect("set");
        assert_eq!(reply["data"]["details"], "Clipping Valorant");
        let reply = connection.set_activity(42, None).expect("clear");
        assert_eq!(reply["evt"], "ERROR");
        // The close frame ends the connection...
        let deadline = Instant::now() + Duration::from_secs(5);
        let err = loop {
            match connection.poll() {
                Err(err) => break err,
                Ok(()) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(()) => panic!("still connected"),
            }
        };
        assert!(err.to_string().contains("bye"), "{err}");
        // ...and so does a pipe whose other end went away.
        done.send(()).unwrap();
        discord.join().expect("server");
        let err = connection.poll().expect_err("broken pipe");
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn handles_refusals_and_absence() {
        let prefix = format!(r"\\.\pipe\krak-test-ipc-{}-refused-", std::process::id());
        assert_eq!(
            Connection::open(&prefix, "123").err().map(|e| e.kind()),
            Some(io::ErrorKind::NotFound),
            "no Discord"
        );
        let mut server = serve(&format!("{prefix}0"));
        let (done, finished) = std::sync::mpsc::channel::<()>();
        let discord = std::thread::spawn(move || {
            read_frame(&mut server);
            write_frame(
                &mut server,
                OP_CLOSE,
                json!({ "code": 4000, "message": "Invalid Client ID" }),
            );
            let _ = finished.recv_timeout(Duration::from_secs(5));
        });
        let err = Connection::open(&prefix, "123").err().expect("refused");
        assert!(err.to_string().contains("Invalid Client ID"), "{err}");
        done.send(()).unwrap();
        discord.join().expect("server");
    }

    /// The real Discord, if it runs: the handshake only (nothing shows on the profile).
    #[test]
    #[ignore = "talks to the Discord client; run with --ignored"]
    fn discord_handshake_live() {
        match Connection::open(PIPE_PREFIX, CLIENT_ID) {
            Ok(_) => println!("Discord answered the handshake"),
            Err(err) if err.kind() == io::ErrorKind::NotFound => println!("Discord is not running"),
            Err(err) => panic!("handshake: {err}"),
        }
    }

    /// Shows the activity on the real Discord profile for 3 seconds, then clears it.
    #[test]
    #[ignore = "shows a status on the Discord profile for 3 s; run with --ignored"]
    fn discord_activity_live() {
        let mut connection = match Connection::open(PIPE_PREFIX, CLIENT_ID) {
            Ok(connection) => connection,
            Err(err) => {
                println!("Discord not available: {err}");
                return;
            }
        };
        let activity = Activity::new(en(), None, Capture::Ready, 0, unix_seconds());
        let pid = std::process::id();
        let reply = connection.set_activity(pid, Some(&activity));
        std::thread::sleep(Duration::from_secs(3));
        let cleared = connection.set_activity(pid, None);
        let reply = reply.expect("set");
        println!("set: {reply}");
        println!("clear: {:?}", cleared.as_ref().map(|r| r["evt"].clone()));
        assert_ne!(reply["evt"], "ERROR", "{reply}");
        assert_ne!(cleared.expect("clear")["evt"], "ERROR");
    }
}

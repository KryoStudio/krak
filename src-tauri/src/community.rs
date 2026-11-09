//! "Share to the Krak community": the Krak Discord's bot posts a clip in #clips for the user,
//! through its share API. Opt-in, and nothing here touches the network before the user asks to
//! share (or to unlink): the API's address comes from `krakclip.com/api/community`, fetched at
//! that moment.
//!
//! Linking: the app asks the bot for a pairing code (`POST /pair`, with the Discord id the Rich
//! Presence connection already knows, if any). The user confirms it in a Discord DM from the
//! bot (`via: dm`) or with `/share link code:XXXX` in the Krak server (`via: command`), while
//! the app polls `GET /pair/:id` with the poll secret only it knows, and receives a token once.
//! The token is kept encrypted with DPAPI (only this Windows account can read it) in
//! `community.bin`, in the app's data folder, never in plain text.
//!
//! HTTP goes through Rust (reqwest on rustls, like the updater), so the page's CSP stays
//! IPC-only. Errors are [`CommunityError`]s: a code the page translates, and what the server
//! said (its error id, when to retry, its size limit).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};

use crate::state::{AppState, lock};

const COMMUNITY_URL: &str = "https://krakclip.com/api/community";
const USER_AGENT: &str = concat!("Krak/", env!("CARGO_PKG_VERSION"));
const TOKEN_FILE: &str = "community.bin";
/// Separates Krak's DPAPI blobs from any other program's.
const ENTROPY: &[u8] = b"krak-community-token";
/// `community-upload-progress`: [`UploadProgress`].
pub const UPLOAD_PROGRESS: &str = "community-upload-progress";
/// Shares are sized for at most Discord's largest upload, and no limit under 1 MB makes sense.
const MAX_SHARE_BYTES: u64 = 500_000_000;
const MIN_SHARE_BYTES: u64 = 1_000_000;
const TITLE_MAX: usize = 100;
const UPLOAD_CHUNK: usize = 256 * 1024;

pub const OFFLINE: &str = "community_offline";
/// The share API is not open (yet), or failing (5xx).
pub const UNAVAILABLE: &str = "community_unavailable";
/// No token, or the server no longer knows it: link again.
pub const UNLINKED: &str = "community_unlinked";
pub const FORBIDDEN: &str = "community_forbidden";
pub const DUPLICATE: &str = "community_duplicate";
pub const TOO_BIG: &str = "community_too_big";
pub const BAD_FILE: &str = "community_bad_file";
pub const INVALID: &str = "community_invalid";
pub const RATE_LIMITED: &str = "community_rate_limited";
pub const FAILED: &str = "community_failed";
pub const CANCELLED: &str = "cancelled";
/// No pairing in progress (or it is over).
pub const NO_PAIRING: &str = "community_no_pairing";

/// A failure, for the page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommunityError {
    pub code: &'static str,
    /// The server's own error id ("not_member"...), when it gave one.
    pub reason: Option<String>,
    /// Seconds before trying again (rate limits).
    pub retry_after: Option<u64>,
    /// The server's size limit (a file too big).
    pub max_bytes: Option<u64>,
}

impl CommunityError {
    fn new(code: &'static str) -> Self {
        Self {
            code,
            reason: None,
            retry_after: None,
            max_bytes: None,
        }
    }
}

type Result<T> = std::result::Result<T, CommunityError>;

/// `krakclip.com/api/community`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommunityInfo {
    pub invite: Option<String>,
    pub guild_id: Option<String>,
    pub clips_channel_id: Option<String>,
    /// The share API's base (ending in `/app/v1`); `None` while sharing is not open.
    pub api: Option<String>,
}

/// A Discord account, as the bot describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommunityUser {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub global_name: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

/// A pairing as the page shows it (the poll secret stays here).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pairing {
    pub code: String,
    /// "dm" or "command".
    pub via: String,
    /// As the server gave it (an ISO date or a timestamp).
    pub expires_at: Value,
}

/// `GET /pair/:id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum PairStatus {
    Pending,
    Linked { user: CommunityUser },
    Expired,
    Denied,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remaining {
    #[serde(default)]
    pub hour: Option<u32>,
    #[serde(default)]
    pub day: Option<u32>,
}

/// `GET /me`, plus the export limit Krak picks from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Me {
    pub user: Option<CommunityUser>,
    pub can_share: bool,
    pub reason: Option<String>,
    pub max_bytes: u64,
    pub remaining: Remaining,
    /// Game names the bot knows (its tags in #clips).
    pub games: Vec<String>,
    pub clips_url: Option<String>,
    /// What an export for this share must stay under, `None` when the limit is unusable.
    pub share_limit: Option<u64>,
}

/// `POST /clips`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Posted {
    pub thread_id: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UploadProgress {
    pub sent: u64,
    pub total: u64,
}

/// What `community.bin` holds (encrypted).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Stored {
    api: String,
    token: String,
    user: CommunityUser,
}

struct PairingState {
    api: String,
    id: String,
    secret: String,
}

/// Managed state: the API base fetched this session, the pairing in progress, the upload's
/// cancel flag.
#[derive(Default)]
pub struct Community {
    api: Mutex<Option<String>>,
    pairing: Mutex<Option<PairingState>>,
    cancel: Arc<AtomicBool>,
}

// Pure parts --------------------------------------------------------------------------------

/// `krakclip.com/api/community`, with an `api` kept only when it is an https URL (trailing
/// slashes dropped).
fn parse_info(body: &str) -> Option<CommunityInfo> {
    let mut info: CommunityInfo = serde_json::from_str(body).ok()?;
    info.api = info.api.as_deref().and_then(api_base);
    Some(info)
}

fn api_base(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let rest = url.strip_prefix("https://")?;
    let host = rest.split('/').next().unwrap_or_default();
    let clean = !host.is_empty()
        && !host.contains('@')
        && url
            .chars()
            .all(|c| c.is_ascii_graphic() && !matches!(c, '?' | '#' | '"' | '\\'));
    clean.then(|| url.to_string())
}

/// A failed answer: the code for its status, and what the body says.
fn parse_error(status: u16, body: &str) -> CommunityError {
    let code = match status {
        400 | 422 => INVALID,
        401 => UNLINKED,
        403 => FORBIDDEN,
        409 => DUPLICATE,
        413 => TOO_BIG,
        415 => BAD_FILE,
        429 => RATE_LIMITED,
        500..=599 => UNAVAILABLE,
        _ => FAILED,
    };
    let json: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let reason = json["error"]
        .as_str()
        .map(|r| r.chars().take(64).collect::<String>())
        .filter(|r| !r.is_empty());
    CommunityError {
        code,
        reason,
        retry_after: json["retryAfter"]
            .as_f64()
            .map(|s| s.max(0.0).ceil() as u64),
        max_bytes: json["maxBytes"].as_u64(),
    }
}

/// `POST /pair` → the pairing (for the page) and the poll id and secret (for us).
fn parse_pair_start(body: &str) -> Option<(Pairing, String, String)> {
    let json: Value = serde_json::from_str(body).ok()?;
    let text = |key: &str| {
        json[key]
            .as_str()
            .map(str::to_string)
            .filter(|v| !v.is_empty())
    };
    let via = match json["via"].as_str() {
        Some("command") => "command",
        _ => "dm",
    };
    Some((
        Pairing {
            code: text("code")?,
            via: via.into(),
            expires_at: json["expiresAt"].clone(),
        },
        text("pairId")?,
        text("pollSecret")?,
    ))
}

/// `GET /pair/:id` → the status, with the token once linked.
fn parse_pair_status(status: u16, body: &str) -> Option<(PairStatus, Option<String>)> {
    let json: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    match (status, json["status"].as_str()) {
        (202, _) | (200, Some("pending")) => Some((PairStatus::Pending, None)),
        (200, Some("linked")) => {
            let token = json["token"].as_str().filter(|t| !t.is_empty())?;
            let user: CommunityUser = serde_json::from_value(json["user"].clone()).ok()?;
            Some((PairStatus::Linked { user }, Some(token.to_string())))
        }
        (410, Some("denied")) => Some((PairStatus::Denied, None)),
        (410 | 404, _) => Some((PairStatus::Expired, None)),
        _ => None,
    }
}

/// `GET /me`, lenient: missing fields read as nothing.
fn parse_me(body: &str) -> Option<Me> {
    let json: Value = serde_json::from_str(body).ok()?;
    let max_bytes = json["maxBytes"].as_u64().unwrap_or(0);
    Some(Me {
        user: serde_json::from_value(json["user"].clone()).ok(),
        can_share: json["canShare"].as_bool().unwrap_or(false),
        reason: json["reason"].as_str().map(str::to_string),
        max_bytes,
        remaining: serde_json::from_value(json["remaining"].clone()).unwrap_or_default(),
        games: game_names(&json["games"]),
        clips_url: json["clipsUrl"].as_str().map(str::to_string),
        share_limit: share_limit(max_bytes),
    })
}

/// The games the bot knows: names, or objects with a `name`.
fn game_names(games: &Value) -> Vec<String> {
    let mut names: Vec<String> = games
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|g| g.as_str().or_else(|| g["name"].as_str()))
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty())
                .collect()
        })
        .unwrap_or_default();
    names.dedup();
    names
}

/// The limit an export for the community aims under: the server's, within Discord's largest
/// upload; `None` for one too small to hold a clip.
pub fn share_limit(server_max: u64) -> Option<u64> {
    (server_max >= MIN_SHARE_BYTES).then(|| server_max.min(MAX_SHARE_BYTES))
}

/// A title as the bot takes it: trimmed, 1 to 100 characters.
fn clean_title(title: &str) -> Option<String> {
    let title = title.trim();
    let count = title.chars().count();
    (1..=TITLE_MAX).contains(&count).then(|| title.to_string())
}

/// A Discord thread id: digits.
fn valid_thread_id(id: &str) -> bool {
    (1..=32).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit())
}

// The token file ----------------------------------------------------------------------------

fn token_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|dir| dir.join(TOKEN_FILE))
}

fn seal(stored: &Stored) -> io::Result<Vec<u8>> {
    let json = serde_json::to_vec(stored).map_err(io::Error::other)?;
    dpapi::protect(&json).ok_or_else(|| io::Error::other("DPAPI refused to encrypt"))
}

fn unseal(bytes: &[u8]) -> Option<Stored> {
    let json = dpapi::unprotect(bytes)?;
    serde_json::from_slice(&json).ok()
}

fn save_token(path: &Path, stored: &Stored) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("bin.tmp");
    fs::write(&tmp, seal(stored)?)?;
    fs::rename(&tmp, path)
}

fn load_token(path: &Path) -> Option<Stored> {
    unseal(&fs::read(path).ok()?)
}

/// Windows' Data Protection API: data only this Windows account can decrypt.
mod dpapi {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    use super::ENTROPY;

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr().cast_mut(),
        }
    }

    /// Copies a blob DPAPI allocated, then frees it.
    ///
    /// # Safety
    /// `out` must come from a successful `CryptProtectData`/`CryptUnprotectData`.
    unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        if out.pbData.is_null() {
            return Vec::new();
        }
        // SAFETY: DPAPI wrote `cbData` bytes at `pbData` and gave us ownership (LocalAlloc).
        unsafe {
            let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            let _ = LocalFree(Some(HLOCAL(out.pbData.cast())));
            bytes
        }
    }

    pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
        let (input, entropy) = (blob(data), blob(ENTROPY));
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: the input blobs point to live slices DPAPI only reads; it allocates `out`.
        unsafe {
            CryptProtectData(
                &input,
                None,
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
            .ok()?;
            Some(take(out))
        }
    }

    pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
        let (input, entropy) = (blob(data), blob(ENTROPY));
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: as in `protect`.
        unsafe {
            CryptUnprotectData(
                &input,
                None,
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
            .ok()?;
            Some(take(out))
        }
    }
}

// HTTP --------------------------------------------------------------------------------------

fn client() -> Result<&'static reqwest::Client> {
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            // reqwest is built without a crypto provider (like the updater): ring's.
            if rustls::crypto::CryptoProvider::get_default().is_none() {
                let _ = rustls::crypto::ring::default_provider().install_default();
            }
            reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .connect_timeout(Duration::from_secs(15))
                .build()
                .inspect_err(|err| eprintln!("krak: community: no HTTP client ({err})"))
                .ok()
        })
        .as_ref()
        .ok_or_else(|| CommunityError::new(FAILED))
}

fn network(err: &reqwest::Error) -> CommunityError {
    eprintln!("krak: community: {err}");
    if err.is_timeout() || err.is_connect() || err.is_request() {
        CommunityError::new(OFFLINE)
    } else {
        CommunityError::new(FAILED)
    }
}

/// Sends a request and reads its answer as text.
async fn send(request: reqwest::RequestBuilder) -> Result<(u16, String)> {
    let response = request.send().await.map_err(|e| network(&e))?;
    let status = response.status().as_u16();
    let body = response.text().await.map_err(|e| network(&e))?;
    Ok((status, body))
}

fn ok_or_error(status: u16, body: &str) -> Result<()> {
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(parse_error(status, body))
    }
}

/// The stored link, or `community_unlinked`.
fn linked(app: &AppHandle) -> Result<(PathBuf, Stored)> {
    let path = token_path(app).ok_or_else(|| CommunityError::new(FAILED))?;
    let stored = load_token(&path).ok_or_else(|| CommunityError::new(UNLINKED))?;
    Ok((path, stored))
}

/// A 401: the server forgot the token, so do we.
fn forget_if_unlinked(path: &Path, err: CommunityError) -> CommunityError {
    if err.code == UNLINKED {
        let _ = fs::remove_file(path);
    }
    err
}

// Commands ----------------------------------------------------------------------------------

/// Where sharing stands (`api: null`: not open yet). Asked when the user clicks Share.
#[tauri::command]
pub async fn community_info(app: AppHandle) -> Result<CommunityInfo> {
    let (status, body) = send(
        client()?
            .get(COMMUNITY_URL)
            .timeout(Duration::from_secs(20)),
    )
    .await?;
    ok_or_error(status, &body)?;
    let info = parse_info(&body).ok_or_else(|| CommunityError::new(UNAVAILABLE))?;
    *lock(&app.state::<Community>().api) = info.api.clone();
    Ok(info)
}

/// The account Krak is linked to, from the token file (no network).
#[tauri::command]
pub async fn community_account(app: AppHandle) -> Result<Option<CommunityUser>> {
    let path = token_path(&app);
    Ok(tauri::async_runtime::spawn_blocking(move || {
        path.and_then(|p| load_token(&p)).map(|s| s.user)
    })
    .await
    .unwrap_or(None))
}

/// Starts linking Krak to the user's Discord account.
#[tauri::command]
pub async fn community_pair(app: AppHandle) -> Result<Pairing> {
    let state = app.state::<Community>();
    let api = lock(&state.api)
        .clone()
        .ok_or_else(|| CommunityError::new(UNAVAILABLE))?;
    // The account of the Discord app on this PC, when Rich Presence knows it: the bot then
    // sends the code to that account directly.
    let handle = app.clone();
    let discord_id = tauri::async_runtime::spawn_blocking(move || {
        crate::presence::info(&handle).user.map(|user| user.id)
    })
    .await
    .unwrap_or(None);
    let body = serde_json::json!({ "discordId": discord_id });
    let request = client()?
        .post(format!("{api}/pair"))
        .json(&body)
        .timeout(Duration::from_secs(20));
    let (status, text) = send(request).await?;
    ok_or_error(status, &text)?;
    let (pairing, id, secret) =
        parse_pair_start(&text).ok_or_else(|| CommunityError::new(FAILED))?;
    *lock(&state.pairing) = Some(PairingState { api, id, secret });
    Ok(pairing)
}

/// Where the pairing stands; once linked, the token is stored (encrypted) and forgotten here.
#[tauri::command]
pub async fn community_pair_status(app: AppHandle) -> Result<PairStatus> {
    let state = app.state::<Community>();
    let (api, id, secret) = lock(&state.pairing)
        .as_ref()
        .map(|p| (p.api.clone(), p.id.clone(), p.secret.clone()))
        .ok_or_else(|| CommunityError::new(NO_PAIRING))?;
    let request = client()?
        .get(format!("{api}/pair/{id}"))
        .bearer_auth(secret)
        .timeout(Duration::from_secs(20));
    let (status, body) = send(request).await?;
    let (pair, token) = match parse_pair_status(status, &body) {
        Some(found) => found,
        None => return Err(parse_error(status, &body)),
    };
    if !matches!(pair, PairStatus::Pending) {
        *lock(&state.pairing) = None;
    }
    if let (PairStatus::Linked { user }, Some(token)) = (&pair, token) {
        let path = token_path(&app).ok_or_else(|| CommunityError::new(FAILED))?;
        let stored = Stored {
            api,
            token,
            user: user.clone(),
        };
        tauri::async_runtime::spawn_blocking(move || save_token(&path, &stored))
            .await
            .map_err(|_| CommunityError::new(FAILED))?
            .map_err(|err| {
                eprintln!("krak: community: cannot store the token: {err}");
                CommunityError::new(FAILED)
            })?;
    }
    Ok(pair)
}

/// What the linked account may share, and how big.
#[tauri::command]
pub async fn community_me(app: AppHandle) -> Result<Me> {
    let (path, stored) = linked(&app)?;
    let request = client()?
        .get(format!("{}/me", stored.api))
        .bearer_auth(&stored.token)
        .timeout(Duration::from_secs(20));
    let (status, body) = send(request).await?;
    ok_or_error(status, &body).map_err(|e| forget_if_unlinked(&path, e))?;
    parse_me(&body).ok_or_else(|| CommunityError::new(FAILED))
}

/// Posts a file an export wrote this session in #clips, with progress events.
#[tauri::command]
pub async fn community_upload(
    app: AppHandle,
    path: String,
    title: String,
    game: Option<String>,
    spoiler: bool,
) -> Result<Posted> {
    let file = PathBuf::from(path);
    // Only what Krak exported for this: never any other file the page names.
    let known = lock(&app.state::<AppState>().exports).files.contains(&file);
    if !known || !file.is_file() {
        return Err(CommunityError::new(BAD_FILE));
    }
    let title = clean_title(&title).ok_or_else(|| CommunityError::new(INVALID))?;
    let game = game
        .map(|g| g.trim().chars().take(TITLE_MAX).collect::<String>())
        .filter(|g| !g.is_empty());
    let (token_file, stored) = linked(&app)?;

    // Read as it is sent, a chunk at a time: a big file is never held in memory whole.
    let reader = fs::File::open(&file).map_err(|_| CommunityError::new(BAD_FILE))?;
    let total = reader
        .metadata()
        .map_err(|_| CommunityError::new(BAD_FILE))?
        .len();
    let cancel = app.state::<Community>().cancel.clone();
    cancel.store(false, Ordering::SeqCst);
    let emitter = app.clone();
    let chunks = std::iter::from_fn({
        let mut reader = reader;
        let mut sent = 0u64;
        move || {
            if cancel.load(Ordering::SeqCst) {
                return Some(Err(io::Error::other(CANCELLED)));
            }
            let mut chunk = vec![0u8; UPLOAD_CHUNK];
            match io::Read::read(&mut reader, &mut chunk) {
                Ok(0) => None,
                Ok(n) => {
                    chunk.truncate(n);
                    sent += n as u64;
                    let _ = emitter.emit(UPLOAD_PROGRESS, UploadProgress { sent, total });
                    Some(Ok(chunk))
                }
                Err(err) => Some(Err(err)),
            }
        }
    });
    let stream = futures_util::stream::iter(chunks);
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| n.is_ascii())
        .unwrap_or("clip.mp4")
        .to_string();
    let part =
        reqwest::multipart::Part::stream_with_length(reqwest::Body::wrap_stream(stream), total)
            .file_name(name)
            .mime_str("video/mp4")
            .map_err(|_| CommunityError::new(FAILED))?;
    let mut form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("title", title)
        .text("spoiler", spoiler.to_string());
    if let Some(game) = game {
        form = form.text("game", game);
    }
    let request = client()?
        .post(format!("{}/clips", stored.api))
        .bearer_auth(&stored.token)
        .multipart(form)
        .timeout(Duration::from_secs(15 * 60));
    let result = send(request).await;
    if app
        .state::<Community>()
        .cancel
        .swap(false, Ordering::SeqCst)
    {
        return Err(CommunityError::new(CANCELLED));
    }
    let (status, body) = result?;
    ok_or_error(status, &body).map_err(|e| forget_if_unlinked(&token_file, e))?;
    serde_json::from_str(&body).map_err(|_| CommunityError::new(FAILED))
}

/// Stops the upload in progress.
#[tauri::command]
pub async fn community_cancel_upload(app: AppHandle) -> Result<()> {
    app.state::<Community>()
        .cancel
        .store(true, Ordering::SeqCst);
    Ok(())
}

/// Deletes a post the user made from Krak.
#[tauri::command]
pub async fn community_delete(app: AppHandle, thread_id: String) -> Result<()> {
    if !valid_thread_id(&thread_id) {
        return Err(CommunityError::new(INVALID));
    }
    let (path, stored) = linked(&app)?;
    let request = client()?
        .delete(format!("{}/clips/{thread_id}", stored.api))
        .bearer_auth(&stored.token)
        .timeout(Duration::from_secs(20));
    let (status, body) = send(request).await?;
    ok_or_error(status, &body).map_err(|e| forget_if_unlinked(&path, e))
}

/// Unlinks Krak: the server forgets the token (when it can be reached), and so do we.
#[tauri::command]
pub async fn community_unlink(app: AppHandle) -> Result<()> {
    let Some(path) = token_path(&app) else {
        return Ok(());
    };
    if let Some(stored) = load_token(&path) {
        let request = client()?
            .delete(format!("{}/token", stored.api))
            .bearer_auth(&stored.token)
            .timeout(Duration::from_secs(10));
        if let Err(err) = send(request).await {
            eprintln!("krak: community: unlinking offline ({})", err.code);
        }
    }
    match fs::remove_file(&path) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => {
            eprintln!("krak: community: cannot remove the token: {err}");
            Err(CommunityError::new(FAILED))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_community_info() {
        let info = parse_info(
            r#"{"invite":"https://discord.gg/krak","guildId":"1","clipsChannelId":"2",
                "api":"https://bot.krakclip.com/app/v1/"}"#,
        )
        .unwrap();
        assert_eq!(info.api.as_deref(), Some("https://bot.krakclip.com/app/v1"));
        assert_eq!(info.guild_id.as_deref(), Some("1"));
        // Not open yet: everything may be null.
        let soon = parse_info(r#"{"invite":null,"guildId":null,"clipsChannelId":null,"api":null}"#);
        assert_eq!(soon, Some(CommunityInfo::default()));
        // Only an https API gets the token.
        for bad in [
            "http://bot.krakclip.com/app/v1",
            "https://",
            "https://user@evil.example/app/v1",
            "javascript:alert(1)",
            "https://bot.krakclip.com/app/v1?x=1",
            "https://bot krakclip.com",
        ] {
            let body = serde_json::json!({ "api": bad }).to_string();
            assert_eq!(parse_info(&body).unwrap().api, None, "{bad}");
        }
        assert!(parse_info("<html>").is_none());
    }

    #[test]
    fn maps_errors() {
        let e = parse_error(429, r#"{"error":"rate_limited","retryAfter":90.5}"#);
        assert_eq!(
            (e.code, e.reason.as_deref(), e.retry_after),
            (RATE_LIMITED, Some("rate_limited"), Some(91))
        );
        let e = parse_error(413, r#"{"error":"too_big","maxBytes":20000000}"#);
        assert_eq!((e.code, e.max_bytes), (TOO_BIG, Some(20_000_000)));
        assert_eq!(parse_error(401, "").code, UNLINKED);
        assert_eq!(
            parse_error(403, r#"{"error":"not_member"}"#)
                .reason
                .as_deref(),
            Some("not_member")
        );
        assert_eq!(parse_error(409, "{}").code, DUPLICATE);
        assert_eq!(parse_error(415, "{}").code, BAD_FILE);
        assert_eq!(parse_error(400, "{}").code, INVALID);
        assert_eq!(parse_error(422, "{}").code, INVALID);
        assert_eq!(parse_error(502, "Bad gateway").code, UNAVAILABLE);
        assert_eq!(parse_error(418, "{}").code, FAILED);
        let long = format!(r#"{{"error":"{}"}}"#, "x".repeat(500));
        assert_eq!(parse_error(400, &long).reason.unwrap().len(), 64);
    }

    #[test]
    fn reads_pairings() {
        let (pairing, id, secret) = parse_pair_start(
            r#"{"pairId":"p1","code":"K7QX","pollSecret":"s3cret","via":"command",
                "expiresAt":"2026-11-09T05:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(
            (pairing.code.as_str(), pairing.via.as_str()),
            ("K7QX", "command")
        );
        assert_eq!((id.as_str(), secret.as_str()), ("p1", "s3cret"));
        assert_eq!(pairing.expires_at, "2026-11-09T05:00:00Z");
        // The secret never reaches the page.
        let json = serde_json::to_string(&pairing).unwrap();
        assert!(!json.contains("s3cret") && !json.contains("p1"));
        // Anything but "command" is a DM; missing parts are no pairing.
        let dm = parse_pair_start(r#"{"pairId":"p","code":"C","pollSecret":"s","expiresAt":1}"#);
        assert_eq!(dm.unwrap().0.via, "dm");
        assert!(parse_pair_start(r#"{"pairId":"p","code":"C"}"#).is_none());

        assert_eq!(
            parse_pair_status(202, r#"{"status":"pending"}"#),
            Some((PairStatus::Pending, None))
        );
        let user =
            r#"{"id":"42","username":"ace","globalName":"Ace","avatarUrl":"https://cdn/a.png"}"#;
        let linked = format!(r#"{{"status":"linked","token":"tok","user":{user}}}"#);
        let (status, token) = parse_pair_status(200, &linked).unwrap();
        assert_eq!(token.as_deref(), Some("tok"));
        let PairStatus::Linked { user } = status else {
            panic!("linked");
        };
        assert_eq!(
            (user.id.as_str(), user.global_name.as_deref()),
            ("42", Some("Ace"))
        );
        assert_eq!(
            parse_pair_status(410, r#"{"status":"expired"}"#),
            Some((PairStatus::Expired, None))
        );
        assert_eq!(
            parse_pair_status(410, r#"{"status":"denied"}"#),
            Some((PairStatus::Denied, None))
        );
        // Linked without a token is not linked; other answers are errors.
        assert!(
            parse_pair_status(
                200,
                r#"{"status":"linked","user":{"id":"1","username":"a"}}"#
            )
            .is_none()
        );
        assert!(parse_pair_status(401, "{}").is_none());
        assert_eq!(
            serde_json::to_value(PairStatus::Expired).unwrap(),
            serde_json::json!({ "status": "expired" })
        );
    }

    #[test]
    fn reads_what_the_account_may_share() {
        let me = parse_me(
            r#"{"user":{"id":"42","username":"ace"},"canShare":true,"maxBytes":20971520,
                "remaining":{"hour":4,"day":19},"games":["Valorant",{"name":"Rocket League","tag":"9"},"",3],
                "clipsUrl":"https://discord.com/channels/1/2"}"#,
        )
        .unwrap();
        assert!(me.can_share);
        assert_eq!(me.games, ["Valorant", "Rocket League"]);
        assert_eq!(
            me.remaining,
            Remaining {
                hour: Some(4),
                day: Some(19)
            }
        );
        assert_eq!(me.share_limit, Some(20_971_520));
        let refused = parse_me(r#"{"canShare":false,"reason":"not_member"}"#).unwrap();
        assert!(!refused.can_share && refused.share_limit.is_none());
        assert_eq!(refused.reason.as_deref(), Some("not_member"));
        assert!(parse_me("nope").is_none());
    }

    #[test]
    fn picks_the_export_limit() {
        assert_eq!(share_limit(20 * 1024 * 1024), Some(20_971_520));
        assert_eq!(share_limit(8_000_000_000), Some(MAX_SHARE_BYTES));
        assert_eq!(share_limit(999_999), None);
        assert_eq!(share_limit(0), None);
    }

    #[test]
    fn checks_titles_and_threads() {
        assert_eq!(
            clean_title("  Ace on Haven  ").as_deref(),
            Some("Ace on Haven")
        );
        assert_eq!(clean_title("   "), None);
        assert_eq!(
            clean_title(&"é".repeat(100)).map(|t| t.chars().count()),
            Some(100)
        );
        assert_eq!(clean_title(&"é".repeat(101)), None);
        assert!(valid_thread_id("1234567890123456789"));
        assert!(!valid_thread_id("12/../token") && !valid_thread_id(""));
    }

    #[test]
    fn keeps_the_token_encrypted() {
        let dir = std::env::temp_dir().join(format!("krak-community-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join(TOKEN_FILE);
        let stored = Stored {
            api: "https://bot.krakclip.com/app/v1".into(),
            token: "very-secret-token".into(),
            user: CommunityUser {
                id: "42".into(),
                username: "ace".into(),
                global_name: Some("Ace".into()),
                avatar_url: None,
            },
        };
        save_token(&path, &stored).expect("save");
        let raw = fs::read(&path).unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(
            !text.contains("very-secret-token") && !text.contains("ace"),
            "encrypted"
        );
        assert_eq!(load_token(&path), Some(stored));
        // Tampered or foreign data reads as no link.
        let mut broken = raw.clone();
        let last = broken.len() - 1;
        broken[last] ^= 0xFF;
        fs::write(&path, broken).unwrap();
        assert_eq!(load_token(&path), None);
        assert_eq!(unseal(b"not dpapi"), None);
        let _ = fs::remove_dir_all(&dir);
    }
}

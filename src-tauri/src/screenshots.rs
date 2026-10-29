//! Screenshots, connected to the app: the hotkey, the tray and `take_screenshot` save the
//! current frame (`capture::screenshot`: from the running capture, else a one-shot duplication)
//! as a PNG under `<clipsDir>/Screenshots/` (`<Game>/<Game> <stamp>.png`, or `<Desktop>
//! <stamp>.png` at its root, like clips), then emit `screenshot-saved` (Screenshot). And the
//! screenshots library: list, delete (Recycle Bin), copy (the image on the clipboard), reveal.
//!
//! Ids are paths relative to the clips folder with forward slashes, like shard ids:
//! `Screenshots/Valorant/Valorant 2026-10-29 20-15-03.png`. The folder is never a game of
//! the Glacier (`library` skips it).

use std::collections::BTreeSet;
use std::fs::{self, Metadata};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_opener::OpenerExt;

use crate::capture::screenshot::{self, Image};
use crate::capture::{self, Stamp, naming};
use crate::media::mp4_writer::{part_path, rename_no_replace};
use crate::media::{ComGuard, png};
use crate::replay::{self, CAPTURE_FAILED};
use crate::state::{AppState, lock};
use crate::{clipboard, i18n, library, shell};

pub const SCREENSHOT_SAVED: &str = "screenshot-saved";
/// The screenshots folder, inside the clips folder.
pub const FOLDER: &str = "Screenshots";
/// A held key repeats: presses closer than this are one.
const DEBOUNCE: Duration = Duration::from_millis(400);
/// How long a screenshot waits for a capture that is starting before taking its own.
const STARTING_WAIT: Duration = Duration::from_secs(2);
const WRITE_FAILED: &str = "write_failed";
const ENCODE_FAILED: &str = "encode_failed";
const INTERNAL: &str = "internal";

type CmdResult<T> = Result<T, String>;

/// `list_screenshots`, `screenshot-saved`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Screenshot {
    pub id: String,
    pub path: String,
    /// Game subfolder, `None` at the root (desktop).
    pub game: Option<String>,
    /// ms since the epoch.
    pub created_at: u64,
    pub size: u64,
    pub width: u32,
    pub height: u32,
}

static LAST_PRESS: Mutex<Option<Instant>> = Mutex::new(None);

/// Hotkey / tray: takes a screenshot in the background. The file arrives as
/// `screenshot-saved`, a failure as `capture-failed` (the in-game notification shows both).
pub fn request(app: &AppHandle) {
    {
        let mut last = lock(&LAST_PRESS);
        let now = Instant::now();
        if last.is_some_and(|at| now.duration_since(at) < DEBOUNCE) {
            return;
        }
        *last = Some(now);
    }
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("kryo-screenshot".into())
        .spawn(move || {
            if let Err(code) = take(&app) {
                let _ = app.emit(CAPTURE_FAILED, code);
            }
        });
    if spawned.is_err() {
        eprintln!("krak: cannot start a screenshot thread");
    }
}

/// Takes, saves and announces a screenshot (blocking: run it off the main thread).
fn take(app: &AppHandle) -> Result<Screenshot, &'static str> {
    let started = Instant::now();
    let settings = app.state::<AppState>().settings();
    // The game and the time of the press.
    let game = capture::game::foreground_game();
    let stamp = Stamp::now();
    let image = grab(app).map_err(|err| {
        eprintln!("krak: screenshot failed: {err}");
        err.code
    })?;
    let _com = ComGuard::new();
    let bytes = png::encode_bgr(image.width, image.height, &image.bgr).map_err(|err| {
        eprintln!("krak: PNG encoding failed: {err}");
        ENCODE_FAILED
    })?;
    drop(image);
    let clips_dir = PathBuf::from(&settings.clips_dir);
    let label = i18n::labels(settings.language.as_deref()).desktop;
    let path = write_png(
        &clips_dir.join(FOLDER),
        game.as_deref(),
        label,
        stamp,
        &bytes,
    )?;
    eprintln!(
        "krak: screenshot {} ({} KB, {:.0} ms)",
        path.display(),
        bytes.len() / 1024,
        started.elapsed().as_secs_f64() * 1000.0
    );
    if settings.capture_sound {
        capture::sound::play_chime();
    }
    let shot = screenshot_at(&clips_dir, &path).ok_or(WRITE_FAILED)?;
    let _ = app.emit(SCREENSHOT_SAVED, shot.clone());
    Ok(shot)
}

/// The running capture's image (waiting a little while it starts), else one of our own.
fn grab(app: &AppHandle) -> crate::media::MediaResult<Image> {
    let deadline = Instant::now() + STARTING_WAIT;
    loop {
        let (source, starting) = replay::with_engine(app, |engine| {
            engine.map_or((None, false), |e| (e.screen(), e.is_starting()))
        });
        if let Some(source) = source {
            match source.read() {
                Ok(image) => return Ok(image),
                Err(err) => {
                    eprintln!("krak: screenshot from the capture failed ({err}), retrying");
                    break;
                }
            }
        }
        if !starting || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    screenshot::one_shot()
}

/// Writes the PNG through `<name>.png.part` (synced, then renamed) under a free name.
fn write_png(
    dir: &Path,
    game: Option<&str>,
    desktop_label: &str,
    stamp: Stamp,
    bytes: &[u8],
) -> Result<PathBuf, &'static str> {
    let folder = match game {
        Some(game) => dir.join(game),
        None => dir.to_path_buf(),
    };
    fs::create_dir_all(&folder).map_err(|_| WRITE_FAILED)?;
    let mut number = 1;
    for _ in 0..20 {
        let (path, n) = naming::free_screenshot_path(dir, game, desktop_label, stamp, number);
        let part = part_path(&path);
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&part)
        {
            Ok(file) => file,
            // Another one took the name meanwhile: next number.
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                number = n + 1;
                continue;
            }
            Err(err) => {
                eprintln!("krak: cannot write {}: {err}", part.display());
                return Err(WRITE_FAILED);
            }
        };
        let written = file.write_all(bytes).and_then(|()| file.sync_all());
        drop(file);
        match written.and_then(|()| rename_no_replace(&part, &path)) {
            Ok(()) => return Ok(path),
            Err(err) => {
                let _ = fs::remove_file(&part);
                if err.kind() != ErrorKind::AlreadyExists {
                    eprintln!("krak: cannot write {}: {err}", path.display());
                    return Err(WRITE_FAILED);
                }
                number = n + 1;
            }
        }
    }
    Err(WRITE_FAILED)
}

/// Checks the shape of an id without touching the disk: `Screenshots/<file>.png` or
/// `Screenshots/<Game>/<file>.png`, plain components only (see `library::parse_id`).
pub fn parse_id(id: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = id.split('/').collect();
    if !(2..=3).contains(&parts.len()) || parts[0] != FOLDER {
        return None;
    }
    let valid = |part: &&str| {
        !part.is_empty()
            && *part != "."
            && *part != ".."
            && !part
                .chars()
                .any(|c| c.is_control() || "\\:<>\"|?*".contains(c))
    };
    let file = parts.last()?;
    let png = Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("png"));
    (parts.iter().all(valid) && png).then(|| parts.iter().collect())
}

/// Resolves an id to an existing screenshot inside the clips folder's `Screenshots` (even
/// through a junction or symlink nothing outside it): `not_found` otherwise.
fn resolve_id(clips_dir: &Path, id: &str) -> Result<PathBuf, &'static str> {
    let rel = parse_id(id).ok_or(library::NOT_FOUND)?;
    let path = clips_dir.join(rel);
    let root = fs::canonicalize(clips_dir.join(FOLDER)).map_err(|_| library::NOT_FOUND)?;
    let real = fs::canonicalize(&path).map_err(|_| library::NOT_FOUND)?;
    if real.starts_with(&root) && real.is_file() {
        Ok(path)
    } else {
        Err(library::NOT_FOUND)
    }
}

/// The screenshot at `path` (inside `<clipsDir>/Screenshots`).
fn screenshot_at(clips_dir: &Path, path: &Path) -> Option<Screenshot> {
    let meta = fs::metadata(path).ok()?;
    let id = replay::shard_id(clips_dir, path)?;
    let parts: Vec<&str> = id.split('/').collect();
    let game = (parts.len() == 3).then(|| parts[1].to_string());
    build(path.to_path_buf(), id, game, &meta)
}

fn build(path: PathBuf, id: String, game: Option<String>, meta: &Metadata) -> Option<Screenshot> {
    let mut head = [0u8; 24];
    fs::File::open(&path).ok()?.read_exact(&mut head).ok()?;
    let (width, height) = png::dimensions(&head)?;
    Some(Screenshot {
        path: path.to_string_lossy().into_owned(),
        id,
        game,
        created_at: library::created_ms(meta),
        size: meta.len(),
        width,
        height,
    })
}

/// Every screenshot, newest first: PNGs at the root of `Screenshots` (desktop) and in its
/// first-level game folders, no symlinks or junctions.
pub fn scan(clips_dir: &Path) -> Vec<Screenshot> {
    let dir = clips_dir.join(FOLDER);
    let entries = |dir: &Path| -> Vec<(PathBuf, String, Metadata)> {
        fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let meta = fs::symlink_metadata(entry.path()).ok()?;
                let name = entry.file_name().into_string().ok()?;
                (!meta.is_symlink()).then(|| (entry.path(), name, meta))
            })
            .collect()
    };
    let is_png = |path: &Path| {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("png"))
    };
    let mut shots = Vec::new();
    for (path, name, meta) in entries(&dir) {
        if meta.is_file() && is_png(&path) {
            shots.extend(build(path, format!("{FOLDER}/{name}"), None, &meta));
        } else if meta.is_dir() && !name.starts_with(['.', '$']) {
            for (file, file_name, file_meta) in entries(&path) {
                if file_meta.is_file() && is_png(&file) {
                    let id = format!("{FOLDER}/{name}/{file_name}");
                    shots.extend(build(file, id, Some(name.clone()), &file_meta));
                }
            }
        }
    }
    shots.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    shots
}

/// Runs blocking work (disk, COM) on a fresh thread and awaits it.
async fn on_thread<T: Send + 'static>(
    name: &str,
    f: impl FnOnce() -> Result<T, &'static str> + Send + 'static,
) -> CmdResult<T> {
    let (tx, mut rx) = tauri::async_runtime::channel(1);
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _ = tx.blocking_send(f());
        })
        .map_err(|_| INTERNAL.to_string())?;
    rx.recv()
        .await
        .ok_or_else(|| INTERNAL.to_string())?
        .map_err(String::from)
}

/// Same as the hotkey, but waits: the file arrives as `screenshot-saved`, a failure is the
/// error ("capture_failed", "no_display", "write_failed", "encode_failed").
#[tauri::command]
pub async fn take_screenshot(app: AppHandle) -> CmdResult<()> {
    on_thread("kryo-screenshot", move || take(&app).map(drop)).await
}

#[tauri::command]
pub async fn list_screenshots(app: AppHandle) -> CmdResult<Vec<Screenshot>> {
    on_thread("kryo-screenshots", move || {
        Ok(scan(&app.state::<AppState>().clips_dir()))
    })
    .await
}

/// Moves screenshots to the Recycle Bin (never a hard delete). Ids that no longer exist are
/// skipped; game folders left empty go too.
#[tauri::command]
pub async fn delete_screenshots(app: AppHandle, ids: Vec<String>) -> CmdResult<()> {
    let dir = app.state::<AppState>().clips_dir();
    let paths: Vec<PathBuf> = ids
        .iter()
        .filter_map(|id| resolve_id(&dir, id).ok())
        .collect();
    if paths.is_empty() {
        return Ok(());
    }
    let root = dir.join(FOLDER);
    let folders: BTreeSet<PathBuf> = paths
        .iter()
        .filter_map(|path| path.parent())
        .filter(|parent| *parent != root)
        .map(Path::to_path_buf)
        .collect();
    let trashed = shell::isolated(move || trash::delete_all(paths))
        .await
        .ok_or(INTERNAL)
        .and_then(|result| result.map_err(|_| "delete_failed"));
    for folder in folders {
        // Only succeeds on an empty folder: anything else in it is left alone.
        let _ = fs::remove_dir(folder);
    }
    trashed.map_err(String::from)
}

/// Puts the image itself on the clipboard (Ctrl+V in Discord, a document, an image editor).
#[tauri::command]
pub async fn copy_screenshot(app: AppHandle, id: String) -> CmdResult<()> {
    let path = resolve_id(&app.state::<AppState>().clips_dir(), &id)?;
    shell::isolated(move || {
        let bytes = fs::read(&path).map_err(|_| library::NOT_FOUND)?;
        let _com = ComGuard::new();
        let (width, height, bgr) = png::decode_bgr(&bytes).map_err(|_| "copy_failed")?;
        clipboard::copy_image(&bytes, width, height, &bgr)
    })
    .await
    .ok_or(INTERNAL)?
    .map_err(String::from)
}

/// Opens Explorer with the screenshot selected.
#[tauri::command]
pub async fn reveal_screenshot(app: AppHandle, id: String) -> CmdResult<()> {
    let path = resolve_id(&app.state::<AppState>().clips_dir(), &id)?;
    let opener = app.clone();
    shell::isolated(move || opener.opener().reveal_item_in_dir(path))
        .await
        .ok_or(INTERNAL)?
        .map_err(|_| "reveal_failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAMP: Stamp = Stamp {
        year: 2026,
        month: 10,
        day: 29,
        hour: 21,
        minute: 4,
        second: 9,
    };

    /// Temporary clips folder, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("kryo-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("temp dir");
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A valid 2x1 PNG made by hand (not through WIC).
    fn tiny_png() -> Vec<u8> {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&2u32.to_be_bytes());
        png.extend_from_slice(&1u32.to_be_bytes());
        png.extend_from_slice(&[8, 2, 0, 0, 0, 0, 0, 0, 0]);
        png
    }

    #[test]
    fn parses_only_screenshot_ids() {
        assert_eq!(
            parse_id("Screenshots/Valorant/Ace.png"),
            Some(Path::new("Screenshots").join("Valorant").join("Ace.png"))
        );
        assert!(parse_id("Screenshots/Desktop 2026-10-29 21-04-09.PNG").is_some());
        for id in [
            "",
            "Valorant/Ace.png",
            "Screenshots",
            "Screenshots/a/b/c.png",
            "Screenshots/../clip.png",
            "Screenshots/Game/clip.mp4",
            "screenshots/Game/a.png",
            "Screenshots//a.png",
            r"Screenshots/..\a.png",
            "Screenshots/C:a.png",
        ] {
            assert_eq!(parse_id(id), None, "{id}");
        }
    }

    #[test]
    fn writes_lists_and_resolves_screenshots() {
        let tmp = TempDir::new("shots");
        let clips = &tmp.0;
        let dir = clips.join(FOLDER);
        let png = tiny_png();
        let a = write_png(&dir, Some("Rocket League"), "Bureau", STAMP, &png).expect("write");
        assert_eq!(
            a,
            dir.join("Rocket League")
                .join("Rocket League 2026-10-29 21-04-09.png")
        );
        // Same second: numbered, like clips. On the desktop: at the root.
        let b = write_png(&dir, Some("Rocket League"), "Bureau", STAMP, &png).expect("write");
        assert!(b.ends_with("Rocket League 2026-10-29 21-04-09 (2).png"));
        let c = write_png(&dir, None, "Bureau", STAMP, &png).expect("write");
        assert_eq!(c, dir.join("Bureau 2026-10-29 21-04-09.png"));
        assert!(!part_path(&c).exists());
        // Not screenshots: other files, nested folders.
        fs::write(dir.join("notes.txt"), b"x").unwrap();
        fs::create_dir_all(dir.join("Game").join("deep")).unwrap();
        fs::write(dir.join("Game").join("deep").join("x.png"), &png).unwrap();

        let shots = scan(clips);
        let mut ids: Vec<&str> = shots.iter().map(|s| s.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(
            ids,
            [
                "Screenshots/Bureau 2026-10-29 21-04-09.png",
                "Screenshots/Rocket League/Rocket League 2026-10-29 21-04-09 (2).png",
                "Screenshots/Rocket League/Rocket League 2026-10-29 21-04-09.png",
            ]
        );
        let shot = screenshot_at(clips, &a).expect("shot");
        assert_eq!(shot.game.as_deref(), Some("Rocket League"));
        assert_eq!(
            (shot.width, shot.height, shot.size),
            (2, 1, png.len() as u64)
        );
        assert!(shot.created_at > 0);
        assert_eq!(screenshot_at(clips, &c).and_then(|s| s.game), None);
        assert_eq!(
            serde_json::to_value(&shot).unwrap()["createdAt"],
            serde_json::json!(shot.created_at)
        );

        assert_eq!(resolve_id(clips, &shot.id), Ok(a));
        assert_eq!(
            resolve_id(clips, "Screenshots/Rocket League/missing.png"),
            Err(library::NOT_FOUND)
        );
        // A screenshot is never a clip of the Glacier, nor the folder a game.
        assert!(library::clip_files(clips).is_empty());
    }
}

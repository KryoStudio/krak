//! Clips folder usage (`get_storage_info`) and the storage limit: once the library is over
//! `storageLimitGb`, its oldest clips that are not favorites go to the Recycle Bin until it
//! fits. Runs after each save and when the setting changes; never touches the clip just saved,
//! a favorite, or anything that is not a clip of the library (`library::clip_files`).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::library;
use crate::state::{AppState, lock};
use crate::watcher::LIBRARY_CHANGED;

/// "GB" as Windows Explorer shows them (2^30 bytes), so the limit matches what users see.
const GB: f64 = (1u64 << 30) as f64;

/// `get_storage_info`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageInfo {
    /// Total size of the library's clips.
    pub used_bytes: u64,
    pub clip_count: u64,
    /// Free space of the clips folder's drive (for this user).
    pub free_bytes: u64,
}

pub fn info(clips_dir: &Path) -> StorageInfo {
    let clips = library::clip_files(clips_dir);
    StorageInfo {
        used_bytes: clips.iter().map(|(_, _, _, meta)| meta.len()).sum(),
        clip_count: clips.len() as u64,
        free_bytes: free_space(clips_dir).unwrap_or(0),
    }
}

/// Bytes available to the user on the drive holding `dir`.
fn free_space(dir: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    use windows::core::PCWSTR;
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
    let mut available = 0u64;
    // SAFETY: NUL-terminated path; out pointer valid for the call.
    unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&mut available), None, None) }.ok()?;
    Some(available)
}

/// A clip as the cleanup sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub size: u64,
    /// ms since the epoch.
    pub created: u64,
}

/// The clips to recycle so the library fits in `limit` bytes: oldest first (by creation, then
/// id), skipping favorites and `keep`. If even that is not enough, it removes what it can.
pub fn select(
    clips: &[Entry],
    favorites: &BTreeSet<String>,
    keep: Option<&str>,
    limit: u64,
) -> Vec<String> {
    let mut total: u64 = clips.iter().map(|c| c.size).sum();
    if total <= limit {
        return Vec::new();
    }
    let mut candidates: Vec<&Entry> = clips
        .iter()
        .filter(|c| !favorites.contains(&c.id) && Some(c.id.as_str()) != keep)
        .collect();
    candidates.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)));
    let mut out = Vec::new();
    for clip in candidates {
        if total <= limit {
            break;
        }
        total = total.saturating_sub(clip.size);
        out.push(clip.id.clone());
    }
    out
}

/// Limit in bytes for a setting in GB.
pub fn limit_bytes(gb: f64) -> u64 {
    (gb * GB).max(0.0) as u64
}

static RUNNING: AtomicBool = AtomicBool::new(false);

/// Enforces the storage limit in the background (no-op without one). `keep` is the clip just
/// saved (id). Emits `library-changed` when something went to the Recycle Bin.
pub fn enforce(app: &AppHandle, keep: Option<String>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if RUNNING.swap(true, Ordering::SeqCst) {
            return; // the running pass sees the newest state anyway
        }
        let result = run(&app, keep).await;
        RUNNING.store(false, Ordering::SeqCst);
        match result {
            Ok(0) => {}
            Ok(count) => {
                eprintln!("krak: storage limit: {count} clip(s) moved to the Recycle Bin");
                let _ = app.emit(LIBRARY_CHANGED, ());
            }
            Err(err) => eprintln!("krak: storage limit: {err}"),
        }
    });
}

async fn run(app: &AppHandle, keep: Option<String>) -> Result<usize, &'static str> {
    let state = app.state::<AppState>();
    let settings = state.settings();
    let Some(gb) = settings.storage_limit_gb else {
        return Ok(0);
    };
    let dir = PathBuf::from(&settings.clips_dir);
    let favorites = lock(&state.meta).favorites.clone();
    let files = library::clip_files(&dir);
    let entries: Vec<Entry> = files
        .iter()
        .map(|(_, id, _, meta)| Entry {
            id: id.clone(),
            size: meta.len(),
            created: library::created_ms(meta),
        })
        .collect();
    let chosen = select(&entries, &favorites, keep.as_deref(), limit_bytes(gb));
    if chosen.is_empty() {
        return Ok(0);
    }
    let paths: Vec<PathBuf> = files
        .iter()
        .filter(|(_, id, _, _)| chosen.contains(id))
        .map(|(path, ..)| path.clone())
        .collect();
    let folders: BTreeSet<PathBuf> = paths
        .iter()
        .filter_map(|p| p.parent())
        .filter(|parent| *parent != dir)
        .map(Path::to_path_buf)
        .collect();
    let count = paths.len();
    crate::shell::isolated(move || trash::delete_all(paths))
        .await
        .ok_or("internal")?
        .map_err(|_| "delete_failed")?;
    for folder in folders {
        // Only succeeds on an empty folder: anything else in it is left alone.
        let _ = fs::remove_dir(folder);
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, size: u64, created: u64) -> Entry {
        Entry {
            id: id.into(),
            size,
            created,
        }
    }

    #[test]
    fn selects_the_oldest_non_favorites_until_it_fits() {
        let clips = [
            entry("Game/c.mp4", 300, 30),
            entry("a.mp4", 100, 10),
            entry("Game/b.mp4", 200, 20),
            entry("d.mp4", 400, 40),
        ];
        let none = BTreeSet::new();
        assert!(select(&clips, &none, None, 1000).is_empty(), "fits already");
        assert_eq!(select(&clips, &none, None, 900), ["a.mp4"]);
        assert_eq!(select(&clips, &none, None, 750), ["a.mp4", "Game/b.mp4"]);
        // Favorites and the clip just saved are never chosen.
        let favorites = BTreeSet::from(["a.mp4".to_string()]);
        assert_eq!(
            select(&clips, &favorites, Some("Game/b.mp4"), 750),
            ["Game/c.mp4"]
        );
        // Not enough to remove: everything allowed goes, nothing else.
        assert_eq!(
            select(&clips, &favorites, Some("d.mp4"), 0),
            ["Game/b.mp4", "Game/c.mp4"]
        );
        // Same creation time: by id, so the choice is stable.
        let twins = [entry("y.mp4", 10, 5), entry("x.mp4", 10, 5)];
        assert_eq!(select(&twins, &none, None, 10), ["x.mp4"]);
    }

    #[test]
    fn converts_gb_like_explorer() {
        assert_eq!(limit_bytes(1.0), 1 << 30);
        assert_eq!(limit_bytes(0.5), 1 << 29);
    }

    #[test]
    fn measures_the_library_only() {
        let dir = std::env::temp_dir().join(format!("kryo-storage-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("Game")).unwrap();
        fs::write(dir.join("a.mp4"), [0u8; 100]).unwrap();
        fs::write(dir.join("Game").join("b.mkv"), [0u8; 50]).unwrap();
        fs::write(dir.join("notes.txt"), [0u8; 999]).unwrap();
        let info = info(&dir);
        assert_eq!((info.used_bytes, info.clip_count), (150, 2));
        assert!(info.free_bytes > 0);
        assert_eq!(
            serde_json::to_value(info).unwrap()["usedBytes"],
            serde_json::json!(150)
        );
        let _ = fs::remove_dir_all(&dir);
    }
}

//! Tauri commands called by the frontend. Errors are short snake_case codes the UI translates.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

use crate::capture::CaptureStatus;
use crate::capture::audio::{self, Microphone};
use crate::capture::encoders::{self, EncoderInfo};
use crate::capture::quality::{self, ResolvedVideo, Tier};
use crate::editor::export::{self, Destination};
use crate::editor::waveform::{self, Waveform};
use crate::editor::{
    EXPORT_PROGRESS, ExportProgress, ExportRequest, ExportResult, ExportSlot, INVALID_REQUEST,
    Progress, ShardInfo, Target, WRITE_FAILED, naming,
};
use crate::library::{self, NOT_FOUND, Shard};
use crate::media::{ComGuard, MfGuard};
use crate::settings::{Settings, SettingsPatch};
use crate::state::{AppState, lock};
use crate::storage::{self, StorageInfo};
use crate::{clipboard, editor, i18n, names, presence, replay, shell, update, window};

type CmdResult<T> = Result<T, String>;

const INTERNAL: &str = "internal";

/// Runs filesystem-heavy work on the blocking pool.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, &'static str> + Send + 'static,
) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|_| INTERNAL.to_string())?
        .map_err(String::from)
}

/// Runs Media Foundation work on a fresh thread with COM (MTA) and Media Foundation started,
/// so no runtime or shell thread ever gets its COM apartment changed.
async fn media_thread<T: Send + 'static>(
    name: &str,
    f: impl FnOnce() -> Result<T, &'static str> + Send + 'static,
) -> CmdResult<T> {
    let (tx, mut rx) = tauri::async_runtime::channel(1);
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let result = {
                let _com = ComGuard::new();
                match MfGuard::new() {
                    Ok(_mf) => f(),
                    Err(_) => Err(editor::UNSUPPORTED),
                }
            };
            let _ = tx.blocking_send(result);
        })
        .map_err(|_| INTERNAL.to_string())?;
    rx.recv()
        .await
        .ok_or_else(|| INTERNAL.to_string())?
        .map_err(String::from)
}

#[tauri::command]
pub async fn get_settings(app: AppHandle) -> CmdResult<Settings> {
    Ok(app.state::<AppState>().settings())
}

#[tauri::command]
pub async fn update_settings(app: AppHandle, patch: SettingsPatch) -> CmdResult<Settings> {
    update::apply(&app, patch).await.map_err(String::from)
}

/// Native folder picker; `None` when cancelled.
#[tauri::command]
pub async fn choose_clips_dir(app: AppHandle) -> CmdResult<Option<Settings>> {
    let current = app.state::<AppState>().settings();
    let (tx, mut rx) = tauri::async_runtime::channel(1);
    let mut dialog = app
        .dialog()
        .file()
        .set_title(i18n::labels(current.language.as_deref()).choose_clips_dir)
        .set_directory(&current.clips_dir);
    if let Some(main) = app.get_webview_window(window::MAIN) {
        dialog = dialog.set_parent(&main);
    }
    dialog.pick_folder(move |picked| {
        let _ = tx.try_send(picked);
    });
    let Some(Some(picked)) = rx.recv().await else {
        return Ok(None);
    };
    let dir = picked
        .into_path()
        .map_err(|_| update::INVALID_DIR.to_string())?;
    let patch = SettingsPatch {
        clips_dir: Some(dir.to_string_lossy().into_owned()),
        ..SettingsPatch::default()
    };
    update::apply(&app, patch)
        .await
        .map(Some)
        .map_err(String::from)
}

#[tauri::command]
pub async fn list_shards(app: AppHandle) -> CmdResult<Vec<Shard>> {
    blocking(move || {
        let state = app.state::<AppState>();
        let dir = state.clips_dir();
        let favorites = lock(&state.meta).favorites.clone();
        Ok(library::scan(&dir, &favorites, &mut lock(&state.durations)))
    })
    .await
}

/// Renames the clip file (same folder, same extension) and carries its favorite over.
#[tauri::command]
pub async fn rename_shard(app: AppHandle, id: String, title: String) -> CmdResult<Shard> {
    blocking(move || {
        let state = app.state::<AppState>();
        let dir = state.clips_dir();
        let source = library::resolve_id(&dir, &id)?;
        let stem = names::sanitize_title(&title).ok_or("invalid_name")?;
        let extension = source
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        let file_name = format!("{stem}.{extension}");
        let target = source.with_file_name(&file_name);

        if target != source {
            // A case-only rename points at the same file on Windows and is allowed.
            if target.exists() && !same_file(&source, &target) {
                return Err("exists");
            }
            fs::rename(&source, &target).map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => NOT_FOUND,
                std::io::ErrorKind::AlreadyExists => "exists",
                _ => "rename_failed",
            })?;
        }

        let new_id = library::sibling_id(&id, &file_name);
        let favorites = {
            let mut meta = lock(&state.meta);
            if new_id != id && meta.favorites.remove(&id) {
                meta.favorites.insert(new_id.clone());
                let _ = meta.save(&state.meta_path);
            }
            meta.favorites.clone()
        };
        library::shard(&dir, &new_id, &favorites, &mut lock(&state.durations))
    })
    .await
}

#[tauri::command]
pub async fn set_favorite(app: AppHandle, id: String, favorite: bool) -> CmdResult<Shard> {
    blocking(move || {
        let state = app.state::<AppState>();
        let dir = state.clips_dir();
        library::resolve_id(&dir, &id)?;
        let favorites = {
            let mut meta = lock(&state.meta);
            let changed = if favorite {
                meta.favorites.insert(id.clone())
            } else {
                meta.favorites.remove(&id)
            };
            if changed && meta.save(&state.meta_path).is_err() {
                // Keep memory and disk in sync.
                if favorite {
                    meta.favorites.remove(&id);
                } else {
                    meta.favorites.insert(id.clone());
                }
                return Err("save_failed");
            }
            meta.favorites.clone()
        };
        library::shard(&dir, &id, &favorites, &mut lock(&state.durations))
    })
    .await
}

/// Moves clips to the Recycle Bin (never a hard delete). Ids that no longer exist are skipped.
#[tauri::command]
pub async fn delete_shards(app: AppHandle, ids: Vec<String>) -> CmdResult<()> {
    let state = app.state::<AppState>();
    let dir = state.clips_dir();
    let paths: Vec<_> = ids
        .iter()
        .filter_map(|id| library::resolve_id(&dir, id).ok())
        .collect();
    // Game folders that may be left empty: removed afterwards so the folder stays tidy.
    let folders: BTreeSet<PathBuf> = paths
        .iter()
        .filter_map(|path| path.parent())
        .filter(|parent| *parent != dir)
        .map(Path::to_path_buf)
        .collect();
    let trashed = if paths.is_empty() {
        Ok(())
    } else {
        shell::isolated(move || trash::delete_all(paths))
            .await
            .ok_or(INTERNAL)
            .and_then(|result| result.map_err(|_| "delete_failed"))
    };

    // Forget metadata of every requested clip that is gone, even after a partial failure.
    let mut meta = lock(&state.meta);
    let before = meta.favorites.len();
    meta.favorites
        .retain(|fav| !ids.contains(fav) || library::resolve_id(&dir, fav).is_ok());
    if meta.favorites.len() != before {
        let _ = meta.save(&state.meta_path);
    }
    drop(meta);
    for folder in folders {
        // Only succeeds on an empty folder: anything else in it is left alone.
        let _ = fs::remove_dir(folder);
    }
    trashed.map_err(String::from)
}

/// Opens Explorer with the clip selected.
#[tauri::command]
pub async fn reveal_shard(app: AppHandle, id: String) -> CmdResult<()> {
    let path = library::resolve_id(&app.state::<AppState>().clips_dir(), &id)?;
    let opener = app.clone();
    shell::isolated(move || opener.opener().reveal_item_in_dir(path))
        .await
        .ok_or(INTERNAL)?
        .map_err(|_| "reveal_failed".to_string())
}

#[tauri::command]
pub async fn open_clips_dir(app: AppHandle) -> CmdResult<()> {
    let dir = app.state::<AppState>().clips_dir();
    shell::open_dir(&app, dir).await.map_err(String::from)
}

/// Puts the clip file itself on the clipboard.
#[tauri::command]
pub async fn copy_shard(app: AppHandle, id: String) -> CmdResult<()> {
    let path = library::resolve_id(&app.state::<AppState>().clips_dir(), &id)?;
    shell::isolated(move || clipboard::copy_file(&path))
        .await
        .ok_or(INTERNAL)?
        .map_err(String::from)
}

#[tauri::command]
pub async fn get_capture_status(app: AppHandle) -> CmdResult<CaptureStatus> {
    Ok(replay::status(&app))
}

/// Pausing stops capture and clears the replay buffer; resuming starts it again.
#[tauri::command]
pub async fn set_capture_paused(app: AppHandle, paused: bool) -> CmdResult<CaptureStatus> {
    Ok(replay::set_paused(&app, paused))
}

/// Same as the hotkey: returns at once, the result comes as `shard-saved` / `capture-failed`.
#[tauri::command]
pub async fn save_replay(app: AppHandle) -> CmdResult<()> {
    replay::request_save(&app);
    Ok(())
}

/// Duration, size, frame rate and audio tracks of a clip, for the editor.
#[tauri::command]
pub async fn probe_shard(app: AppHandle, id: String) -> CmdResult<ShardInfo> {
    let path = library::resolve_id(&app.state::<AppState>().clips_dir(), &id)?;
    media_thread("kryo-probe", move || editor::probe::probe(&path)).await
}

/// Exports a trimmed, re-mixed clip: a copy next to the original (`copy`), a file where the
/// user chose (`file`, from `choose_export_path`), or a file under `maxBytes` put on the
/// clipboard (`discord`); with `watermark`, the "by krak" badge burnt in. Progress comes as
/// `export-progress` events.
#[tauri::command]
pub async fn export_shard(app: AppHandle, request: ExportRequest) -> CmdResult<ExportResult> {
    // Held until the result is ready (clipboard included).
    let slot = ExportSlot::acquire()?;
    let state = app.state::<AppState>();
    let clips_dir = state.clips_dir();
    let source = library::resolve_id(&clips_dir, &request.id)?;
    let destination = match request.target {
        Target::Copy => Destination::Copy {
            label: i18n::labels(state.settings().language.as_deref())
                .edited
                .to_string(),
        },
        Target::File => Destination::File {
            path: naming::export_file(request.path.as_deref().ok_or(INVALID_REQUEST)?)?,
        },
        Target::Discord => {
            let max_bytes = request
                .max_bytes
                .filter(|m| m.is_finite() && *m >= 1.0)
                .ok_or(INVALID_REQUEST)?;
            let dir = app
                .path()
                .app_cache_dir()
                .map_err(|_| WRITE_FAILED)?
                .join("exports");
            Destination::Discord {
                dir,
                max_bytes: max_bytes as u64,
            }
        }
    };
    let job = export::Request {
        start: request.start,
        end: request.end,
        gains: request.gains,
        destination,
        watermark: request.watermark,
        options: editor::transcode::Options::default(),
    };
    let cancel = slot.cancel_flag();
    let emitter = app.clone();
    let id = request.id.clone();
    let exported = media_thread("kryo-export", move || {
        let report = move |progress: f64| {
            let payload = ExportProgress {
                id: id.clone(),
                progress,
            };
            let _ = emitter.emit(EXPORT_PROGRESS, payload);
        };
        let progress = Progress::new(cancel, &report);
        let started = std::time::Instant::now();
        let exported = export::export(&source, &job, &progress)?;
        eprintln!(
            "krak: exported {} ({} bytes, {}) in {:.1} s",
            exported.path.display(),
            exported.size,
            exported.detail,
            started.elapsed().as_secs_f64()
        );
        Ok(exported)
    })
    .await?;

    let shard = match request.target {
        Target::Copy => {
            let file_name = exported
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            let new_id = library::sibling_id(&request.id, &file_name);
            let app = app.clone();
            // The file is written: a failed lookup only means the UI refreshes on its own.
            blocking(move || {
                let state = app.state::<AppState>();
                let favorites = lock(&state.meta).favorites.clone();
                Ok(
                    library::shard(&clips_dir, &new_id, &favorites, &mut lock(&state.durations))
                        .ok(),
                )
            })
            .await?
        }
        Target::File => {
            let (app, path) = (app.clone(), exported.path.clone());
            // In the clips folder (or one of its game folders), the file is a shard too.
            blocking(move || {
                let state = app.state::<AppState>();
                let Some(id) = shard_id_of(&clips_dir, &path) else {
                    return Ok(None);
                };
                let favorites = lock(&state.meta).favorites.clone();
                Ok(library::shard(&clips_dir, &id, &favorites, &mut lock(&state.durations)).ok())
            })
            .await?
        }
        Target::Discord => {
            let path = exported.path.clone();
            shell::isolated(move || clipboard::copy_file(&path))
                .await
                .ok_or(INTERNAL)?
                .map_err(String::from)?;
            None
        }
    };
    lock(&state.exports).files.insert(exported.path.clone());
    drop(slot);
    Ok(ExportResult {
        path: exported.path.to_string_lossy().into_owned(),
        size: exported.size,
        shard,
        reencoded: exported.reencoded,
    })
}

/// The capture display and what this PC can encode and decode (cached after the first call).
#[tauri::command]
pub async fn get_encoder_info() -> CmdResult<EncoderInfo> {
    media_thread("kryo-encoders", || {
        encoders::info().map_err(|err| {
            eprintln!("krak: encoder info: {err}");
            err.code
        })
    })
    .await
}

/// What the engine would record with the current settings and `patch` (nothing is applied).
#[tauri::command]
pub async fn resolve_video(app: AppHandle, patch: SettingsPatch) -> CmdResult<ResolvedVideo> {
    let settings = app.state::<AppState>().settings().merge(patch);
    let video = replay::video_settings(&settings);
    media_thread("kryo-encoders", move || {
        let (display, refresh, caps) = encoders::detect().map_err(|err| err.code)?;
        let plan = quality::plan(
            &video,
            (display.width, display.height),
            refresh,
            &caps,
            Tier::Best,
        );
        Ok(plan.resolved)
    })
    .await
}

/// Active microphones (WASAPI capture endpoints), the default one first.
#[tauri::command]
pub async fn list_microphones() -> CmdResult<Vec<Microphone>> {
    media_thread("kryo-microphones", audio::microphones).await
}

/// Size and count of the library's clips, and the free space of their drive.
#[tauri::command]
pub async fn get_storage_info(app: AppHandle) -> CmdResult<StorageInfo> {
    blocking(move || Ok(storage::info(&app.state::<AppState>().clips_dir()))).await
}

/// One row of audio peaks per track of a clip (same order and names as `probe_shard`).
#[tauri::command]
pub async fn get_waveforms(app: AppHandle, id: String, buckets: f64) -> CmdResult<Vec<Waveform>> {
    let path = library::resolve_id(&app.state::<AppState>().clips_dir(), &id)?;
    let buckets = if buckets.is_finite() {
        buckets.round().clamp(0.0, 1e6) as usize
    } else {
        0
    };
    media_thread("kryo-waveform", move || {
        waveform::waveforms(&path, buckets).map(|w| w.as_ref().clone())
    })
    .await
}

/// Stops the running export (it then fails with `cancelled` and leaves no file behind).
#[tauri::command]
pub async fn cancel_export() -> CmdResult<()> {
    editor::cancel();
    Ok(())
}

/// Native "Save as" for a file export (MP4), opening where the last one was saved this
/// session, else in Videos. The dialog asks before overwriting. `None` when cancelled.
#[tauri::command]
pub async fn choose_export_path(
    app: AppHandle,
    suggested_name: String,
) -> CmdResult<Option<String>> {
    let state = app.state::<AppState>();
    let last_dir = lock(&state.exports).last_dir.clone();
    let dir = last_dir
        .filter(|dir| dir.is_dir())
        .or_else(|| app.path().video_dir().ok().filter(|dir| dir.is_dir()));
    let name = naming::suggested_file_name(&suggested_name, dir.as_deref());
    let (tx, mut rx) = tauri::async_runtime::channel(1);
    let mut dialog = app
        .dialog()
        .file()
        .add_filter("MP4", &["mp4"])
        .set_file_name(name);
    if let Some(dir) = &dir {
        dialog = dialog.set_directory(dir);
    }
    if let Some(main) = app.get_webview_window(window::MAIN) {
        dialog = dialog.set_parent(&main);
    }
    dialog.save_file(move |picked| {
        let _ = tx.try_send(picked);
    });
    let Some(Some(picked)) = rx.recv().await else {
        return Ok(None);
    };
    let picked = picked
        .into_path()
        .map_err(|_| editor::INVALID_DIR.to_string())?;
    let path = naming::with_mp4_extension(picked.clone());
    // ".mp4" added to the name typed: that file was not the one the dialog asked about.
    if path != picked && path.exists() {
        return Err("exists".into());
    }
    lock(&state.exports).last_dir = path.parent().map(Path::to_path_buf);
    Ok(Some(path.to_string_lossy().into_owned()))
}

/// Opens Explorer with a file an export wrote this session selected (nothing else).
#[tauri::command]
pub async fn reveal_export(app: AppHandle, path: String) -> CmdResult<()> {
    let path = PathBuf::from(path);
    let known = lock(&app.state::<AppState>().exports).files.contains(&path);
    if !known || !path.is_file() {
        return Err(NOT_FOUND.into());
    }
    let opener = app.clone();
    shell::isolated(move || opener.opener().reveal_item_in_dir(path))
        .await
        .ok_or(INTERNAL)?
        .map_err(|_| "reveal_failed".to_string())
}

/// Whether the Discord status is on, and whether Discord is there to show it.
#[tauri::command]
pub async fn get_presence(app: AppHandle) -> CmdResult<presence::PresenceInfo> {
    blocking(move || Ok(presence::info(&app))).await
}

/// The shard id of `path` when it is a clip the library lists: a file of the clips folder or
/// of one of its game folders (compared through their real paths, links resolved).
fn shard_id_of(clips_dir: &Path, path: &Path) -> Option<String> {
    let dir = fs::canonicalize(clips_dir).ok()?;
    let file = fs::canonicalize(path).ok()?;
    let parts = file
        .strip_prefix(&dir)
        .ok()?
        .iter()
        .map(|part| part.to_str())
        .collect::<Option<Vec<&str>>>()?;
    match parts.as_slice() {
        [_] => {}
        [folder, _] if !folder.starts_with(['.', '$']) => {}
        _ => return None,
    }
    library::is_video(path).then(|| parts.join("/"))
}

fn same_file(a: &Path, b: &Path) -> bool {
    matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_exported_files_in_the_library() {
        let dir = std::env::temp_dir().join(format!("kryo-commands-ids-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let clips = dir.join("Clips");
        fs::create_dir_all(clips.join("Valorant")).unwrap();
        fs::create_dir_all(clips.join(".hidden")).unwrap();
        fs::create_dir_all(clips.join("Valorant").join("deeper")).unwrap();
        let file = |rel: &str| {
            let path = clips.join(rel);
            fs::write(&path, b"x").unwrap();
            path
        };
        assert_eq!(
            shard_id_of(&clips, &file("Ace.mp4")).as_deref(),
            Some("Ace.mp4")
        );
        assert_eq!(
            shard_id_of(&clips, &file(r"Valorant\Clutch.mp4")).as_deref(),
            Some("Valorant/Clutch.mp4")
        );
        // Spelled differently, same file.
        let other_case = dir.join("CLIPS").join("valorant").join("Clutch.mp4");
        assert_eq!(
            shard_id_of(&clips, &other_case).as_deref(),
            Some("Valorant/Clutch.mp4")
        );
        assert_eq!(shard_id_of(&clips, &file(r".hidden\a.mp4")), None);
        assert_eq!(shard_id_of(&clips, &file(r"Valorant\deeper\a.mp4")), None);
        assert_eq!(shard_id_of(&clips, &file("notes.txt")), None);
        let outside = dir.join("Elsewhere.mp4");
        fs::write(&outside, b"x").unwrap();
        assert_eq!(shard_id_of(&clips, &outside), None);
        let _ = fs::remove_dir_all(&dir);
    }
}

//! Clip library: scanning the clips folder, shard ids, favorites and cached durations.
//!
//! Layout: `<clipsDir>/<file>` (no game) and `<clipsDir>/<Game>/<file>`. A shard id is the path
//! relative to the clips folder with forward slashes, e.g. `Valorant/Ace 2026-10-28 20-15-03.mp4`.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{mp4, names, persist};

pub const NOT_FOUND: &str = "not_found";
const VIDEO_EXTENSIONS: [&str; 4] = ["mp4", "mkv", "mov", "webm"];

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Shard {
    pub id: String,
    pub path: String,
    pub title: Option<String>,
    pub game: Option<String>,
    pub duration: f64,
    pub size: u64,
    pub created_at: u64,
    pub favorite: bool,
}

/// Persisted per-clip metadata (`library.json`).
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryMeta {
    pub favorites: BTreeSet<String>,
}

impl LibraryMeta {
    pub fn load(path: &Path) -> Self {
        match persist::read(path) {
            persist::Read::Text(text) => serde_json::from_str(&text).unwrap_or_else(|_| {
                persist::backup(path);
                Self::default()
            }),
            persist::Read::Missing | persist::Read::Unreadable => Self::default(),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        persist::write_json(path, self)
    }
}

/// In-memory durations keyed by path and invalidated by size/mtime, so rescans stay cheap.
#[derive(Default)]
pub struct DurationCache(HashMap<PathBuf, CachedDuration>);

struct CachedDuration {
    size: u64,
    modified: Option<SystemTime>,
    seconds: f64,
}

impl DurationCache {
    fn get(&mut self, path: &Path, meta: &Metadata) -> f64 {
        let (size, modified) = (meta.len(), meta.modified().ok());
        if let Some(hit) = self.0.get(path)
            && hit.size == size
            && hit.modified == modified
        {
            return hit.seconds;
        }
        let seconds = probe_duration(path);
        let entry = CachedDuration {
            size,
            modified,
            seconds,
        };
        self.0.insert(path.to_path_buf(), entry);
        seconds
    }

    fn retain(&mut self, keep: &HashSet<PathBuf>) {
        self.0.retain(|path, _| keep.contains(path));
    }
}

fn probe_duration(path: &Path) -> f64 {
    match extension(path).as_deref() {
        Some("mp4" | "mov") => mp4::file_duration(path),
        // No Matroska/WebM parser yet: the UI shows the duration as unknown.
        _ => 0.0,
    }
}

fn extension(path: &Path) -> Option<String> {
    path.extension()?.to_str().map(str::to_ascii_lowercase)
}

pub fn is_video(path: &Path) -> bool {
    extension(path).is_some_and(|ext| VIDEO_EXTENSIONS.contains(&ext.as_str()))
}

/// Checks the shape of an id without touching the disk: one or two plain components
/// (`file` or `Game/file`), no `..`, drive, root, backslash or stream syntax, video extension.
pub fn parse_id(id: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = id.split('/').collect();
    if parts.is_empty() || parts.len() > 2 {
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
    if !parts.iter().all(valid) {
        return None;
    }
    let rel: PathBuf = parts.iter().collect();
    is_video(&rel).then_some(rel)
}

/// Resolves an id to an existing clip inside `clips_dir`. Anything that escapes the folder
/// (even through a junction or symlink) or does not exist is reported as `not_found`.
pub fn resolve_id(clips_dir: &Path, id: &str) -> Result<PathBuf, &'static str> {
    let rel = parse_id(id).ok_or(NOT_FOUND)?;
    let path = clips_dir.join(rel);
    let root = fs::canonicalize(clips_dir).map_err(|_| NOT_FOUND)?;
    let real = fs::canonicalize(&path).map_err(|_| NOT_FOUND)?;
    if real.starts_with(&root) && real.is_file() {
        Ok(path)
    } else {
        Err(NOT_FOUND)
    }
}

/// Id of a file renamed to `file_name` next to the clip `id`.
pub fn sibling_id(id: &str, file_name: &str) -> String {
    match id.rsplit_once('/') {
        Some((game, _)) => format!("{game}/{file_name}"),
        None => file_name.to_string(),
    }
}

/// Builds the shard for a single (already resolved) id.
pub fn shard(
    clips_dir: &Path,
    id: &str,
    favorites: &BTreeSet<String>,
    cache: &mut DurationCache,
) -> Result<Shard, &'static str> {
    let path = resolve_id(clips_dir, id)?;
    let meta = fs::metadata(&path).map_err(|_| NOT_FOUND)?;
    let game = id.rsplit_once('/').map(|(game, _)| game.to_string());
    Ok(build(path, id.to_string(), game, &meta, favorites, cache))
}

/// Lists every clip in the clips folder and its first-level game folders, newest first.
pub fn scan(
    clips_dir: &Path,
    favorites: &BTreeSet<String>,
    cache: &mut DurationCache,
) -> Vec<Shard> {
    let mut found: Vec<(PathBuf, String, Option<String>, Metadata)> = Vec::new();
    for (path, name, meta) in entries(clips_dir) {
        if meta.is_file() && is_video(&path) {
            found.push((path, name, None, meta));
        } else if meta.is_dir() && !name.starts_with(['.', '$']) {
            for (file, file_name, file_meta) in entries(&path) {
                if file_meta.is_file() && is_video(&file) {
                    found.push((
                        file,
                        format!("{name}/{file_name}"),
                        Some(name.clone()),
                        file_meta,
                    ));
                }
            }
        }
    }

    cache.retain(&found.iter().map(|(path, ..)| path.clone()).collect());
    let mut shards: Vec<Shard> = found
        .into_iter()
        .map(|(path, id, game, meta)| build(path, id, game, &meta, favorites, cache))
        .collect();
    shards.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    shards
}

/// Regular files and folders of `dir` with UTF-8 names. Symlinks and junctions are skipped
/// so the library never reaches outside the clips folder.
fn entries(dir: &Path) -> impl Iterator<Item = (PathBuf, String, Metadata)> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let meta = fs::symlink_metadata(entry.path()).ok()?;
            if meta.is_symlink() {
                return None;
            }
            let name = entry.file_name().into_string().ok()?;
            Some((entry.path(), name, meta))
        })
}

fn build(
    path: PathBuf,
    id: String,
    game: Option<String>,
    meta: &Metadata,
    favorites: &BTreeSet<String>,
    cache: &mut DurationCache,
) -> Shard {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    Shard {
        title: names::display_title(stem),
        game,
        duration: cache.get(&path, meta),
        size: meta.len(),
        created_at: unix_ms(meta.created().or_else(|_| meta.modified())),
        favorite: favorites.contains(&id),
        path: path.to_string_lossy().into_owned(),
        id,
    }
}

fn unix_ms(time: std::io::Result<SystemTime>) -> u64 {
    time.ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn parses_valid_ids() {
        assert_eq!(parse_id("clip.mp4"), Some(PathBuf::from("clip.mp4")));
        assert_eq!(
            parse_id("Valorant/Ace.MKV"),
            Some(Path::new("Valorant").join("Ace.MKV"))
        );
        assert!(parse_id("Counter-Strike 2/round 2026-10-28 20-15-03.webm").is_some());
    }

    #[test]
    fn rejects_escaping_or_malformed_ids() {
        for id in [
            "",
            "../clip.mp4",
            "a/../../clip.mp4",
            "./clip.mp4",
            "/clip.mp4",
            "C:/clip.mp4",
            "C:clip.mp4",
            r"C:\Windows\clip.mp4",
            r"\\server\share\clip.mp4",
            r"Game\..\..\clip.mp4",
            "a//clip.mp4",
            "Game/",
            "a/b/clip.mp4",
            "clip.mp4:stream",
            "clip.txt",
            "clip",
            "Game/clip.exe",
        ] {
            assert_eq!(parse_id(id), None, "{id}");
        }
    }

    #[test]
    fn resolve_stays_inside_clips_dir() {
        let tmp = TempDir::new("resolve");
        let root = tmp.0.join("clips");
        fs::create_dir_all(root.join("Game")).expect("dirs");
        fs::write(root.join("clip.mp4"), b"x").expect("file");
        fs::write(root.join("Game").join("c.mp4"), b"x").expect("file");
        fs::write(tmp.0.join("outside.mp4"), b"x").expect("file");

        assert_eq!(resolve_id(&root, "clip.mp4"), Ok(root.join("clip.mp4")));
        assert_eq!(
            resolve_id(&root, "Game/c.mp4"),
            Ok(root.join("Game").join("c.mp4"))
        );
        assert_eq!(resolve_id(&root, "../outside.mp4"), Err(NOT_FOUND));
        let absolute = tmp.0.join("outside.mp4").to_string_lossy().into_owned();
        assert_eq!(resolve_id(&root, &absolute), Err(NOT_FOUND));
        assert_eq!(resolve_id(&root, "missing.mp4"), Err(NOT_FOUND));
        assert_eq!(resolve_id(&root, "Game"), Err(NOT_FOUND));
    }

    #[test]
    fn scans_root_and_game_folders() {
        let tmp = TempDir::new("scan");
        let root = &tmp.0;
        fs::create_dir_all(root.join("Apex").join("nested")).expect("dirs");
        fs::write(root.join("Desk 2026-10-28 20-15-03.mp4"), b"not really mp4").expect("file");
        fs::write(root.join("Apex").join("Clutch.mkv"), b"x").expect("file");
        fs::write(root.join("Apex").join("notes.txt"), b"x").expect("file");
        fs::write(root.join("Apex").join("nested").join("deep.mp4"), b"x").expect("file");

        let favorites = BTreeSet::from(["Apex/Clutch.mkv".to_string()]);
        let mut cache = DurationCache::default();
        let mut shards = scan(root, &favorites, &mut cache);
        shards.sort_by(|a, b| a.id.cmp(&b.id));

        let ids: Vec<&str> = shards.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["Apex/Clutch.mkv", "Desk 2026-10-28 20-15-03.mp4"]);
        assert_eq!(shards[0].game.as_deref(), Some("Apex"));
        assert_eq!(shards[0].title.as_deref(), Some("Clutch"));
        assert!(shards[0].favorite);
        assert_eq!(shards[1].game, None);
        assert_eq!(shards[1].title, None);
        assert_eq!(shards[1].duration, 0.0);
        assert_eq!(shards[1].size, 14);
        assert!(shards[1].created_at > 0);
        assert_eq!(
            shards[1].path,
            root.join("Desk 2026-10-28 20-15-03.mp4").to_string_lossy()
        );
    }

    #[test]
    fn computes_sibling_ids() {
        assert_eq!(sibling_id("Game/old.mp4", "new.mp4"), "Game/new.mp4");
        assert_eq!(sibling_id("old.mp4", "new.mp4"), "new.mp4");
    }

    #[test]
    fn library_meta_tolerates_corruption() {
        let tmp = TempDir::new("meta");
        let path = tmp.0.join("library.json");
        fs::write(&path, "nope").expect("write");
        assert!(LibraryMeta::load(&path).favorites.is_empty());
        assert!(tmp.0.join("library.json.bak").exists());

        let meta = LibraryMeta {
            favorites: BTreeSet::from(["a.mp4".to_string()]),
        };
        meta.save(&path).expect("save");
        assert_eq!(LibraryMeta::load(&path).favorites, meta.favorites);
    }
}

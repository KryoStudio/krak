//! Making room: old shards recorded at a high bitrate are re-encoded in H.264 at the "standard"
//! quality's bitrate, which plays everywhere and is about half to a quarter of the size of a
//! "high" or "premium" recording.
//!
//! Only what is worth it: shards older than the chosen age, never a favorite, and only those
//! whose bitrate is well above the target. Each one is written next to itself first
//! ("<name>.mp4.krak-new", a name the library ignores) and checked (as long, really smaller);
//! then the original steps aside ("<name>.mp4.krak-old"), the new file takes its name and dates
//! (the Glacier keeps its order; its id, so favorites, edits, markers and collections stay
//! with it), and the caller sends the original to the Recycle Bin. Any failure leaves the
//! original where it was; a file in use (being played) is skipped.

use std::fs::{self, File, FileTimes};
use std::os::windows::fs::FileTimesExt;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::editor::{self, Progress, ShardInfo};
use crate::library::Shard;

/// The "standard" quality: bits per pixel per frame.
const STANDARD_BITS_PER_PIXEL: f64 = 0.06;
const MIN_BITRATE: f64 = 2_000_000.0;
const MAX_BITRATE: f64 = 40_000_000.0;
/// Recompressed only when the clip's bitrate is at least this many times the target.
const WORTH_IT: f64 = 1.6;
/// What the audio tracks weigh after re-encoding (stereo mix and game, mono microphone).
const AUDIO_BITRATE: f64 = 416_000.0;
/// The new file must last as long as the original, within this (s)...
const SAME_LENGTH: f64 = 0.3;
/// ...and be this much smaller at least.
const SMALLER: f64 = 0.92;
/// Skipped: the file is in use (being played) or could not be replaced.
pub const IN_USE: &str = "in_use";

/// A shard worth recompressing.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub id: String,
    pub size: u64,
    /// Its size once recompressed, roughly.
    pub estimate: u64,
}

/// The H.264 bitrate a clip of this size and rate is recompressed at.
pub fn target_bitrate(width: u32, height: u32, fps: f64) -> f64 {
    let fps = if fps.is_finite() && fps > 0.0 {
        fps.min(144.0)
    } else {
        60.0
    };
    (f64::from(width) * f64::from(height) * fps * STANDARD_BITS_PER_PIXEL)
        .clamp(MIN_BITRATE, MAX_BITRATE)
}

/// The size after recompression of a clip of `size` bytes described by `info`, `None` when it
/// would not shrink enough to be worth it.
pub fn estimate(info: &ShardInfo, size: u64) -> Option<u64> {
    if !info.duration.is_finite() || info.duration < 1.0 || info.width == 0 || info.height == 0 {
        return None;
    }
    let target = target_bitrate(info.width, info.height, info.fps);
    let current = size as f64 * 8.0 / info.duration;
    if current < target * WORTH_IT {
        return None;
    }
    Some(((target + AUDIO_BITRATE) * info.duration / 8.0) as u64)
}

/// The shards created before `before` (ms since 1970), not favorites, that recompression would
/// make smaller (`probe` reads a clip's size and rate). Oldest first.
pub fn candidates(
    shards: &[Shard],
    before: u64,
    probe: impl Fn(&Path) -> Option<ShardInfo>,
) -> Vec<Candidate> {
    let mut old: Vec<&Shard> = shards
        .iter()
        .filter(|s| !s.favorite && s.created_at > 0 && s.created_at < before)
        .filter(|s| s.path.to_ascii_lowercase().ends_with(".mp4"))
        .collect();
    old.sort_by_key(|s| s.created_at);
    old.into_iter()
        .filter_map(|shard| {
            let info = probe(Path::new(&shard.path))?;
            let estimate = estimate(&info, shard.size)?;
            Some(Candidate {
                id: shard.id.clone(),
                size: shard.size,
                estimate,
            })
        })
        .collect()
}

/// What recompressing one clip did: bytes saved, and the original moved aside (for the
/// Recycle Bin).
pub struct Done {
    pub saved: u64,
    pub original: PathBuf,
}

/// Recompresses the clip at `path` in place (module docs). Needs COM and Media Foundation on
/// the calling thread.
pub fn recompress(path: &Path, progress: &Progress) -> Result<Done, &'static str> {
    let info = editor::probe::probe(path)?;
    let size = fs::metadata(path).map_err(|_| editor::EXPORT_FAILED)?.len();
    let bitrate = target_bitrate(info.width, info.height, info.fps);
    let fresh = sibling(path, "krak-new");
    let aside = sibling(path, "krak-old");
    let result = (|| {
        editor::export::recompress(path, &fresh, bitrate, progress)?;
        // Checked before anything is replaced.
        let new = editor::probe::probe(&fresh)?;
        let new_size = fs::metadata(&fresh)
            .map_err(|_| editor::EXPORT_FAILED)?
            .len();
        if (new.duration - info.duration).abs() > SAME_LENGTH
            || new_size as f64 > size as f64 * SMALLER
        {
            eprintln!(
                "krak: recompress: {} kept ({:.2} s, {size} bytes -> {:.2} s, {new_size} bytes)",
                path.display(),
                info.duration,
                new.duration
            );
            return Err(editor::EXPORT_FAILED);
        }
        replace(path, &fresh, &aside)?;
        Ok(Done {
            saved: size.saturating_sub(new_size),
            original: aside.clone(),
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&fresh);
        let _ = fs::remove_file(crate::media::mp4_writer::part_path(&fresh));
    }
    result
}

/// Puts `fresh` in the place of `path`, with its dates; the original ends at `aside`.
fn replace(path: &Path, fresh: &Path, aside: &Path) -> Result<(), &'static str> {
    let meta = fs::metadata(path).map_err(|_| IN_USE)?;
    let times = FileTimes::new();
    let times = match meta.modified() {
        Ok(t) => times.set_modified(t),
        Err(_) => times,
    };
    let times = match meta.created() {
        Ok(t) => times.set_created(t),
        Err(_) => times,
    };
    // A clip being played cannot be renamed: skipped.
    fs::rename(path, aside).map_err(|_| IN_USE)?;
    if fs::rename(fresh, path).is_err() {
        let _ = fs::rename(aside, path);
        return Err(IN_USE);
    }
    if let Ok(file) = File::options().write(true).open(path) {
        let _ = file.set_times(times);
    }
    Ok(())
}

/// "<name>.mp4.<tag>" next to `path`: not a video name, so the library never lists it.
fn sibling(path: &Path, tag: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(tag);
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(width: u32, height: u32, fps: f64, duration: f64) -> ShardInfo {
        ShardInfo {
            duration,
            width,
            height,
            fps,
            audio_tracks: Vec::new(),
        }
    }

    fn shard(id: &str, size: u64, created_at: u64, favorite: bool) -> Shard {
        Shard {
            id: id.into(),
            path: format!(r"C:\Clips\{id}"),
            title: None,
            game: None,
            duration: 30.0,
            size,
            created_at,
            favorite,
        }
    }

    #[test]
    fn targets_the_standard_quality() {
        // 1080p60: 7.5 Mbit/s.
        assert!((target_bitrate(1920, 1080, 60.0) - 7_464_960.0).abs() < 1.0);
        assert_eq!(target_bitrate(640, 360, 30.0), MIN_BITRATE);
        assert_eq!(target_bitrate(3840, 2160, 144.0), MAX_BITRATE);
        assert!((target_bitrate(1920, 1080, f64::NAN) - 7_464_960.0).abs() < 1.0);
    }

    #[test]
    fn recompresses_only_what_shrinks() {
        let hd = info(1920, 1080, 60.0, 30.0);
        // 30 s at 40 Mbit/s (a premium recording): down to about a fifth.
        let big = 150_000_000;
        let after = estimate(&hd, big).expect("worth it");
        assert!(after < big / 4, "{after}");
        // Already at 9 Mbit/s: not worth it.
        assert_eq!(estimate(&hd, 34_000_000), None);
        // Unknown length or size: never.
        assert_eq!(estimate(&info(1920, 1080, 60.0, 0.0), big), None);
        assert_eq!(estimate(&info(0, 0, 60.0, 30.0), big), None);
    }

    #[test]
    fn picks_old_heavy_shards_but_never_favorites() {
        let day = 24 * 3600 * 1000;
        let now = 100 * day;
        let shards = [
            shard("new.mp4", 150_000_000, now - day, false),
            shard("old.mp4", 150_000_000, now - 60 * day, false),
            shard("older.mp4", 150_000_000, now - 90 * day, false),
            shard("fav.mp4", 150_000_000, now - 90 * day, true),
            shard("light.mp4", 20_000_000, now - 90 * day, false),
            shard("other.mkv", 150_000_000, now - 90 * day, false),
        ];
        let found = candidates(&shards, now - 30 * day, |_| {
            Some(info(1920, 1080, 60.0, 30.0))
        });
        let ids: Vec<&str> = found.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["older.mp4", "old.mp4"], "oldest first");
        assert!(found.iter().all(|c| c.estimate < c.size));
        // A clip that cannot be read is left alone.
        assert!(candidates(&shards, now, |_| None).is_empty());
    }

    #[test]
    fn works_next_to_the_clip_under_names_the_library_ignores() {
        let path = Path::new(r"C:\Clips\Valorant\Ace.mp4");
        assert_eq!(
            sibling(path, "krak-new"),
            Path::new(r"C:\Clips\Valorant\Ace.mp4.krak-new")
        );
    }
}

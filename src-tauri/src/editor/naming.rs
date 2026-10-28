//! Output paths: "<original stem> (<label>).mp4" next to the original for copies (" (2)"... on
//! collisions), `<cache>/exports/<title>.mp4` for Discord, and cleanup of old Discord exports.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::media::mp4_writer;
use crate::names;

/// Discord exports older than this are deleted when the next one starts.
pub const EXPORT_MAX_AGE: Duration = Duration::from_secs(24 * 3600);
/// Candidates tried before giving up on a free name.
pub const MAX_ATTEMPTS: u32 = 999;
/// Every copy suffix the app ever wrote (both languages), so re-editing a copy does not stack
/// them: "clip (edit) (edit).mp4" becomes "clip (edit) (2).mp4".
const LABELS: [&str; 2] = ["edit", "montage"];

/// The stem of a copy of `stem`, candidate number `n` (1 = no number).
pub fn copy_stem(stem: &str, label: &str, n: u32) -> String {
    let base = strip_copy_suffix(stem);
    if n > 1 {
        format!("{base} ({label}) ({n})")
    } else {
        format!("{base} ({label})")
    }
}

/// "clip (edit)", "clip (montage) (3)" -> "clip".
fn strip_copy_suffix(stem: &str) -> &str {
    let mut base = stem;
    // An optional " (N)" after the label.
    if let Some(head) = base.strip_suffix(')')
        && let Some((head, digits)) = head.rsplit_once(" (")
        && !digits.is_empty()
        && digits.len() <= 3
        && digits.bytes().all(|b| b.is_ascii_digit())
        && LABELS.iter().any(|l| head.ends_with(&format!(" ({l})")))
    {
        base = head;
    }
    for label in LABELS {
        if let Some(head) = base.strip_suffix(&format!(" ({label})"))
            && !head.trim().is_empty()
        {
            return head;
        }
    }
    base
}

/// The first free copy path for `source` (neither the file nor its `.part` exists), from
/// candidate `first`, with its number.
pub fn free_copy_path(source: &Path, label: &str, first: u32) -> Option<(PathBuf, u32)> {
    let stem = source.file_stem()?.to_str()?;
    (first.max(1)..=MAX_ATTEMPTS).find_map(|n| {
        let path = source.with_file_name(format!("{}.mp4", copy_stem(stem, label, n)));
        let taken = path.exists() || mp4_writer::part_path(&path).exists();
        (!taken).then_some((path, n))
    })
}

/// File name (without extension) of a Discord export of the clip `stem`.
pub fn export_stem(stem: &str) -> String {
    names::sanitize_title(stem).unwrap_or_else(|| "Kryo Shard".to_string())
}

/// `<dir>/<stem>.mp4`, then `<stem> (2).mp4`...
pub fn export_path(dir: &Path, stem: &str, n: u32) -> PathBuf {
    if n > 1 {
        dir.join(format!("{stem} ({n}).mp4"))
    } else {
        dir.join(format!("{stem}.mp4"))
    }
}

/// Deletes files in `dir` last modified more than `max_age` ago (best effort).
pub fn clean_old_exports(dir: &Path, max_age: Duration) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let old = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > max_age);
        if meta.is_file() && old {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_copies() {
        assert_eq!(
            copy_stem("Valorant 2026-10-28 20-15-03", "montage", 1),
            "Valorant 2026-10-28 20-15-03 (montage)"
        );
        assert_eq!(copy_stem("Ace", "edit", 2), "Ace (edit) (2)");
        // Re-editing a copy: the suffix is replaced, not stacked.
        assert_eq!(copy_stem("Ace (edit)", "edit", 1), "Ace (edit)");
        assert_eq!(copy_stem("Ace (edit) (4)", "edit", 2), "Ace (edit) (2)");
        assert_eq!(copy_stem("Ace (montage) (12)", "edit", 1), "Ace (edit)");
        // Things that only look like suffixes are kept.
        assert_eq!(copy_stem("Clip (2)", "edit", 1), "Clip (2) (edit)");
        assert_eq!(copy_stem("(edit)", "edit", 1), "(edit) (edit)");
        assert_eq!(copy_stem("Ace (edits)", "edit", 1), "Ace (edits) (edit)");
        assert_eq!(
            copy_stem("Ace (edit) (1234)", "edit", 1),
            "Ace (edit) (1234) (edit)"
        );
    }

    #[test]
    fn finds_free_copy_paths() {
        let dir = std::env::temp_dir().join(format!("kryo-editor-names-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("Ace.mkv");
        fs::write(&source, b"x").unwrap();
        let (first, n) = free_copy_path(&source, "edit", 1).unwrap();
        assert_eq!((first.clone(), n), (dir.join("Ace (edit).mp4"), 1));
        fs::write(&first, b"x").unwrap();
        // A copy being written counts as taken too.
        fs::write(dir.join("Ace (edit) (2).mp4.part"), b"x").unwrap();
        assert_eq!(
            free_copy_path(&source, "edit", 1).unwrap(),
            (dir.join("Ace (edit) (3).mp4"), 3)
        );
        // Editing the copy itself lands on the same series.
        assert_eq!(
            free_copy_path(&first, "edit", 1).unwrap(),
            (dir.join("Ace (edit) (3).mp4"), 3)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_discord_exports() {
        assert_eq!(export_stem("Ace: clutch?"), "Ace clutch");
        assert_eq!(export_stem("CON"), "Kryo Shard");
        let dir = Path::new(r"C:\cache\exports");
        assert_eq!(export_path(dir, "Ace", 1), dir.join("Ace.mp4"));
        assert_eq!(export_path(dir, "Ace", 2), dir.join("Ace (2).mp4"));
    }

    #[test]
    fn cleans_only_old_exports() {
        let dir = std::env::temp_dir().join(format!("kryo-editor-clean-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sub")).unwrap();
        let old = dir.join("old.mp4");
        let fresh = dir.join("fresh.mp4");
        fs::write(&old, b"x").unwrap();
        fs::write(&fresh, b"x").unwrap();
        let two_days_ago = SystemTime::now() - Duration::from_secs(48 * 3600);
        fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(two_days_ago)
            .unwrap();
        clean_old_exports(&dir, EXPORT_MAX_AGE);
        assert!(!old.exists());
        assert!(fresh.exists() && dir.join("sub").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}

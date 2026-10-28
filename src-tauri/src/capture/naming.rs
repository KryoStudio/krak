//! Clip file names: `<clipsDir>/<Game>/<Game> YYYY-MM-DD HH-MM-SS.mp4`, or
//! `<clipsDir>/<Desktop> YYYY-MM-DD HH-MM-SS.mp4` without a game, with " (2)", " (3)"... on
//! collision. `names::display_title` treats all of these as automatic names.

use std::path::{Path, PathBuf};

use crate::media::mp4_writer::part_path;
use crate::names;

/// Local wall-clock time of a capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
}

impl Stamp {
    #[cfg(windows)]
    pub fn now() -> Self {
        // SAFETY: no preconditions.
        let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        Self {
            year: t.wYear,
            month: t.wMonth,
            day: t.wDay,
            hour: t.wHour,
            minute: t.wMinute,
            second: t.wSecond,
        }
    }
}

impl std::fmt::Display for Stamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02} {:02}-{:02}-{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// Candidate path number `n` (1 = no suffix). `game` must already be a valid folder name
/// (`names::sanitize_title`); an unusable desktop label falls back to "Desktop".
pub fn clip_path(
    clips_dir: &Path,
    game: Option<&str>,
    desktop_label: &str,
    stamp: Stamp,
    n: u32,
) -> PathBuf {
    let suffix = if n > 1 {
        format!(" ({n})")
    } else {
        String::new()
    };
    match game {
        Some(game) => clips_dir
            .join(game)
            .join(format!("{game} {stamp}{suffix}.mp4")),
        None => {
            let label = names::sanitize_title(desktop_label).unwrap_or_else(|| "Desktop".into());
            clips_dir.join(format!("{label} {stamp}{suffix}.mp4"))
        }
    }
}

/// The first candidate (from number `first`) that exists neither as a clip nor as a clip
/// being written, with its number.
pub fn free_clip_path(
    clips_dir: &Path,
    game: Option<&str>,
    desktop_label: &str,
    stamp: Stamp,
    first: u32,
) -> (PathBuf, u32) {
    let mut n = first.max(1);
    loop {
        let path = clip_path(clips_dir, game, desktop_label, stamp, n);
        if n >= 999 || !(path.exists() || part_path(&path).exists()) {
            return (path, n);
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAMP: Stamp = Stamp {
        year: 2026,
        month: 10,
        day: 8,
        hour: 9,
        minute: 5,
        second: 3,
    };

    #[test]
    fn formats_paths() {
        let dir = Path::new(r"C:\Clips");
        assert_eq!(
            clip_path(dir, Some("Valorant"), "Bureau", STAMP, 1),
            dir.join("Valorant")
                .join("Valorant 2026-10-08 09-05-03.mp4")
        );
        assert_eq!(
            clip_path(dir, None, "Bureau", STAMP, 1),
            dir.join("Bureau 2026-10-08 09-05-03.mp4")
        );
        assert_eq!(
            clip_path(dir, None, "Desktop", STAMP, 3),
            dir.join("Desktop 2026-10-08 09-05-03 (3).mp4")
        );
        assert_eq!(
            clip_path(dir, None, "<>", STAMP, 1),
            dir.join("Desktop 2026-10-08 09-05-03.mp4")
        );
    }

    #[test]
    fn generated_names_are_auto_titles() {
        let dir = Path::new(r"C:\Clips");
        for n in [1, 2, 17] {
            for path in [
                clip_path(dir, Some("Apex Legends"), "Desktop", STAMP, n),
                clip_path(dir, None, "Bureau", STAMP, n),
            ] {
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap();
                assert_eq!(names::display_title(stem), None, "{stem}");
            }
        }
    }

    #[test]
    fn skips_existing_and_in_progress_names() {
        let dir = std::env::temp_dir().join(format!("kryo-naming-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Game")).unwrap();
        let free = || free_clip_path(&dir, Some("Game"), "Desktop", STAMP, 1);
        let first = clip_path(&dir, Some("Game"), "Desktop", STAMP, 1);
        assert_eq!(free(), (first.clone(), 1));
        std::fs::write(&first, b"x").unwrap();
        let second = clip_path(&dir, Some("Game"), "Desktop", STAMP, 2);
        assert_eq!(free(), (second.clone(), 2));
        std::fs::write(part_path(&second), b"x").unwrap();
        assert_eq!(
            free(),
            (clip_path(&dir, Some("Game"), "Desktop", STAMP, 3), 3)
        );
        // Starting further (after a lost race) skips lower numbers.
        assert_eq!(free_clip_path(&dir, Some("Game"), "Desktop", STAMP, 5).1, 5);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

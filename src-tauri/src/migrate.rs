//! Krak was called Kryo Shard (identifier `com.kryostudio.kryoshard`) before. On the first launch
//! under the new identifier, its files are copied over so an update loses nothing: settings,
//! library, window position and the interface's storage (Discord size, saved trims and volumes).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const OLD_IDENTIFIER: &str = "com.kryostudio.kryoshard";

/// What the new folders don't have yet, per Windows folder (Tauri's config and data dirs are
/// `%APPDATA%\<identifier>`, the webview's is under `%LOCALAPPDATA%\<identifier>`).
const ITEMS: [(&str, &[&str]); 2] = [
    (
        "APPDATA",
        &["settings.json", "library.json", ".window-state.json"],
    ),
    ("LOCALAPPDATA", &[r"EBWebView\Default\Local Storage"]),
];

/// Runs before the plugins load their files (the window state is read at startup).
pub fn from_kryo_shard(identifier: &str) {
    for (variable, items) in ITEMS {
        let Some(root) = std::env::var_os(variable).map(PathBuf::from) else {
            continue;
        };
        let (old, new) = (root.join(OLD_IDENTIFIER), root.join(identifier));
        if old == new || !old.is_dir() {
            continue;
        }
        for item in items {
            let target = new.join(item);
            if target.exists() {
                continue;
            }
            if let Err(err) = copy(&old.join(item), &target) {
                eprintln!("krak: cannot carry over {item} from Kryo Shard: {err}");
            }
        }
    }
}

/// Copies a file or a folder tree; a missing source is not an error.
fn copy(from: &Path, to: &Path) -> io::Result<()> {
    if from.is_dir() {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy(&entry.path(), &to.join(entry.file_name()))?;
        }
    } else if from.is_file() {
        if let Some(dir) = to.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::copy(from, to)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_trees_and_skips_missing_sources() {
        let dir = std::env::temp_dir().join(format!("krak-migrate-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let old = dir.join("old");
        fs::create_dir_all(old.join(r"a\b")).expect("tree");
        fs::write(old.join(r"a\b\c.log"), "x").expect("file");
        fs::write(old.join("settings.json"), "{}").expect("file");

        let new = dir.join("new");
        copy(&old.join("a"), &new.join("a")).expect("folder");
        copy(&old.join("settings.json"), &new.join("settings.json")).expect("file");
        copy(&old.join("missing.json"), &new.join("missing.json")).expect("missing source");
        assert_eq!(
            fs::read_to_string(new.join(r"a\b\c.log")).ok().as_deref(),
            Some("x")
        );
        assert!(new.join("settings.json").is_file());
        assert!(!new.join("missing.json").exists());

        let _ = fs::remove_dir_all(&dir);
    }
}

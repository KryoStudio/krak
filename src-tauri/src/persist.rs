//! Small helpers for the app's JSON files: tolerant reads, atomic writes, corrupt-file backups.

use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use serde::Serialize;

/// Result of reading a JSON file from disk.
pub enum Read {
    /// The file does not exist yet (first launch).
    Missing,
    /// The file exists but could not be read (locked, permissions...). Do not overwrite it.
    Unreadable,
    Text(String),
}

pub fn read(path: &Path) -> Read {
    match fs::read_to_string(path) {
        Ok(text) => Read::Text(text),
        Err(err) if err.kind() == ErrorKind::NotFound => Read::Missing,
        Err(_) => Read::Unreadable,
    }
}

/// Keeps a copy of a corrupt file as `<name>.bak` before it gets replaced by defaults.
pub fn backup(path: &Path) {
    let _ = fs::copy(path, sibling(path, "bak"));
}

/// Writes `value` as pretty JSON through a temporary file, so a crash never leaves half a file.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    let tmp = sibling(path, "tmp");
    fs::write(&tmp, json)?;
    fs::rename(&tmp, path)
}

/// `settings.json` -> `settings.json.<suffix>`.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(suffix);
    path.with_file_name(name)
}

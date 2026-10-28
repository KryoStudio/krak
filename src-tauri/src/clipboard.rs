//! Puts a file on the clipboard (CF_HDROP), so Ctrl+V in Explorer or Discord pastes the video.

use std::path::Path;

#[cfg(windows)]
const FAILED: &str = "copy_failed";

#[cfg(windows)]
pub fn copy_file(path: &Path) -> Result<(), &'static str> {
    use clipboard_win::{Clipboard, options::DoClear, raw::set_file_list_with};

    let path = path.to_str().ok_or(FAILED)?;
    // Opening can fail while another app holds the clipboard: retry a few times (5 ms apart).
    let _clipboard = Clipboard::new_attempts(10).map_err(|_| FAILED)?;
    set_file_list_with(&[path], DoClear).map_err(|_| FAILED)
}

#[cfg(not(windows))]
pub fn copy_file(_path: &Path) -> Result<(), &'static str> {
    Err("unsupported")
}

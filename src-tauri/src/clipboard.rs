//! Puts a file on the clipboard (CF_HDROP), so Ctrl+V in Explorer or Discord pastes the video,
//! or an image (a screenshot), so Ctrl+V pastes the picture itself.

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

/// Puts an image on the clipboard as "PNG" (the file's own bytes: browsers, Discord, Office
/// read it losslessly) and CF_DIB (everything else; Windows derives the other bitmap formats).
/// No file (CF_HDROP): Chromium would offer both, and Discord would attach the image twice.
#[cfg(windows)]
pub fn copy_image(png: &[u8], width: u32, height: u32, bgr: &[u8]) -> Result<(), &'static str> {
    use clipboard_win::{Clipboard, formats, raw};

    let dib = dib(width, height, bgr).ok_or(FAILED)?;
    let png_format = raw::register_format("PNG").ok_or(FAILED)?;
    let _clipboard = Clipboard::new_attempts(10).map_err(|_| FAILED)?;
    raw::empty().map_err(|_| FAILED)?;
    raw::set_without_clear(png_format.get(), png).map_err(|_| FAILED)?;
    raw::set_without_clear(formats::CF_DIB, &dib).map_err(|_| FAILED)
}

#[cfg(not(windows))]
pub fn copy_image(_png: &[u8], _width: u32, _height: u32, _bgr: &[u8]) -> Result<(), &'static str> {
    Err("unsupported")
}

/// A CF_DIB: BITMAPINFOHEADER then 24-bit rows, bottom-up, each padded to 4 bytes.
/// `bgr` holds top-down rows, tightly packed. `None` if it is too short or the size absurd.
fn dib(width: u32, height: u32, bgr: &[u8]) -> Option<Vec<u8>> {
    let row = usize::try_from(width).ok()?.checked_mul(3)?;
    let stride = row.div_ceil(4) * 4;
    let rows = usize::try_from(height).ok()?;
    if width == 0 || height == 0 || bgr.len() < row.checked_mul(rows)? {
        return None;
    }
    let image = u32::try_from(stride.checked_mul(rows)?).ok()?;
    let mut out = Vec::with_capacity(40 + image as usize);
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&i32::try_from(width).ok()?.to_le_bytes());
    out.extend_from_slice(&i32::try_from(height).ok()?.to_le_bytes()); // > 0: bottom-up
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&24u16.to_le_bytes()); // bits per pixel
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&image.to_le_bytes());
    out.extend_from_slice(&[0; 16]); // resolution, palette
    for line in bgr[..row * rows].chunks_exact(row).rev() {
        out.extend_from_slice(line);
        out.resize(out.len() + stride - row, 0);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_bottom_up_padded_dibs() {
        // 2x2: top row 1 2, bottom row 3 4 (one byte per channel = pixel number).
        let bgr = [1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4, 4];
        let image = dib(2, 2, &bgr).expect("dib");
        assert_eq!(image.len(), 40 + 2 * 8, "rows padded from 6 to 8 bytes");
        assert_eq!(u32::from_le_bytes(image[20..24].try_into().unwrap()), 16);
        assert_eq!(i32::from_le_bytes(image[8..12].try_into().unwrap()), 2);
        assert_eq!(
            &image[40..48],
            &[3, 3, 3, 4, 4, 4, 0, 0],
            "bottom row first"
        );
        assert_eq!(&image[48..56], &[1, 1, 1, 2, 2, 2, 0, 0]);
        assert!(dib(2, 2, &bgr[1..]).is_none());
        assert!(dib(0, 2, &bgr).is_none());
    }
}

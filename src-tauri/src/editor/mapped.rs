//! Read-only memory mapping of a clip, so a passthrough export hands the muxer slices of the
//! source file without copying hundreds of megabytes into RAM (pages come from the file cache).
//!
//! While mapped, Windows refuses to delete or truncate the file, so the slices stay valid.

use std::fs::File;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, PAGE_READONLY,
    UnmapViewOfFile,
};
use windows::core::PCWSTR;

pub struct MappedFile {
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    mapping: HANDLE,
    len: usize,
    _file: File,
}

// SAFETY: the view is read-only memory owned by this value; nothing mutates it.
unsafe impl Send for MappedFile {}
// SAFETY: as above, shared reads only.
unsafe impl Sync for MappedFile {}

impl MappedFile {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let file = File::open(path)?;
        let len = usize::try_from(file.metadata()?.len())
            .map_err(|_| std::io::Error::other("file too large"))?;
        if len == 0 {
            return Err(std::io::Error::other("empty file"));
        }
        // SAFETY: the file handle is valid for the call; size 0 maps the whole file.
        let mapping = unsafe {
            CreateFileMappingW(
                HANDLE(file.as_raw_handle()),
                None,
                PAGE_READONLY,
                0,
                0,
                PCWSTR::null(),
            )
        }
        .map_err(std::io::Error::other)?;
        // SAFETY: valid mapping handle; 0 bytes maps the whole file.
        let view = unsafe { MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0) };
        if view.Value.is_null() {
            let err = std::io::Error::last_os_error();
            // SAFETY: closes the handle created above.
            let _ = unsafe { CloseHandle(mapping) };
            return Err(err);
        }
        Ok(Self {
            view,
            mapping,
            len,
            _file: file,
        })
    }

    pub fn bytes(&self) -> &[u8] {
        // SAFETY: the view covers `len` readable bytes for as long as `self` lives.
        unsafe { std::slice::from_raw_parts(self.view.Value.cast::<u8>(), self.len) }
    }

    /// `size` bytes at `offset`, `None` past the end of the file.
    pub fn slice(&self, offset: u64, size: u32) -> Option<&[u8]> {
        let start = usize::try_from(offset).ok()?;
        let end = start.checked_add(usize::try_from(size).ok()?)?;
        self.bytes().get(start..end)
    }
}

impl Drop for MappedFile {
    fn drop(&mut self) {
        // SAFETY: unmaps our own view and closes our own mapping handle, once.
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.mapping);
        }
    }
}

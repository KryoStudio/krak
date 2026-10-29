//! PNG through the Windows Imaging Component: screenshots in (8-bit BGR pixels), a decoded
//! copy out for the clipboard, and the image size read straight from a file's header.

#[cfg(windows)]
use super::MediaResult;

/// Size of a PNG from its first 24 bytes (signature and `IHDR`), `None` if it is not one.
pub fn dimensions(head: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if head.get(..8)? != SIGNATURE || head.get(12..16)? != b"IHDR" {
        return None;
    }
    let be32 = |at: usize| Some(u32::from_be_bytes(head.get(at..at + 4)?.try_into().ok()?));
    Some((be32(16)?, be32(20)?))
}

/// Encodes tightly packed 8-bit BGR rows as an RGB PNG. The caller has initialized COM.
#[cfg(windows)]
pub fn encode_bgr(width: u32, height: u32, bgr: &[u8]) -> MediaResult<Vec<u8>> {
    imp::encode(width, height, bgr)
}

/// Decodes a PNG (any kind WIC reads) into top-down, tightly packed 8-bit BGR rows. The
/// caller has initialized COM. Returns (width, height, pixels).
#[cfg(windows)]
pub fn decode_bgr(png: &[u8]) -> MediaResult<(u32, u32, Vec<u8>)> {
    imp::decode(png)
}

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat24bppBGR,
        IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapEncoderNoCache,
        WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
    };
    use windows::Win32::System::Com::StructuredStorage::CreateStreamOnHGlobal;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, CoCreateInstance, STREAM_SEEK_END, STREAM_SEEK_SET,
    };

    use crate::media::{Code, MediaError, MediaResult};

    const FAILED: &str = "encode_failed";

    fn factory() -> MediaResult<IWICImagingFactory> {
        // SAFETY: plain in-process COM creation (the caller initialized COM).
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
            .code(FAILED)
    }

    pub fn encode(width: u32, height: u32, bgr: &[u8]) -> MediaResult<Vec<u8>> {
        let stride = width.checked_mul(3).ok_or(MediaError::new(FAILED))?;
        if width == 0 || height == 0 || bgr.len() < stride as usize * height as usize {
            return Err(MediaError::new(FAILED));
        }
        let factory = factory()?;
        // SAFETY: COM calls on objects created here; WritePixels reads `height` rows of
        // `stride` bytes, which `bgr` holds (checked above).
        unsafe {
            let stream = CreateStreamOnHGlobal(HGLOBAL::default(), true).code(FAILED)?;
            let encoder = factory
                .CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())
                .code(FAILED)?;
            encoder
                .Initialize(&stream, WICBitmapEncoderNoCache)
                .code(FAILED)?;
            let mut frame = None;
            let mut options = None;
            encoder
                .CreateNewFrame(&mut frame, &mut options)
                .code(FAILED)?;
            let frame = frame.ok_or(MediaError::new(FAILED))?;
            frame.Initialize(options.as_ref()).code(FAILED)?;
            frame.SetSize(width, height).code(FAILED)?;
            let mut format = GUID_WICPixelFormat24bppBGR;
            frame.SetPixelFormat(&mut format).code(FAILED)?;
            if format != GUID_WICPixelFormat24bppBGR {
                return Err(MediaError::new(FAILED));
            }
            let bytes = stride as usize * height as usize;
            frame
                .WritePixels(height, stride, &bgr[..bytes])
                .code(FAILED)?;
            frame.Commit().code(FAILED)?;
            encoder.Commit().code(FAILED)?;

            let mut size = 0u64;
            stream
                .Seek(0, STREAM_SEEK_END, Some(&mut size))
                .code(FAILED)?;
            stream.Seek(0, STREAM_SEEK_SET, None).code(FAILED)?;
            let mut out = vec![0u8; usize::try_from(size).map_err(|_| MediaError::new(FAILED))?];
            let mut read = 0u32;
            let len = u32::try_from(out.len()).map_err(|_| MediaError::new(FAILED))?;
            stream
                .Read(out.as_mut_ptr().cast(), len, Some(&mut read))
                .ok()
                .code(FAILED)?;
            out.truncate(read as usize);
            Ok(out)
        }
    }

    pub fn decode(png: &[u8]) -> MediaResult<(u32, u32, Vec<u8>)> {
        let factory = factory()?;
        // SAFETY: COM calls on objects created here; the memory stream reads `png`, which
        // outlives the decoder; CopyPixels writes `height` rows of `stride` bytes into `out`.
        unsafe {
            let stream = factory.CreateStream().code(FAILED)?;
            stream.InitializeFromMemory(png).code(FAILED)?;
            let decoder = factory
                .CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
                .code(FAILED)?;
            let frame = decoder.GetFrame(0).code(FAILED)?;
            let converter = factory.CreateFormatConverter().code(FAILED)?;
            converter
                .Initialize(
                    &frame,
                    &GUID_WICPixelFormat24bppBGR,
                    WICBitmapDitherTypeNone,
                    None,
                    0.0,
                    WICBitmapPaletteTypeCustom,
                )
                .code(FAILED)?;
            let (mut width, mut height) = (0u32, 0u32);
            converter.GetSize(&mut width, &mut height).code(FAILED)?;
            let stride = width.checked_mul(3).ok_or(MediaError::new(FAILED))?;
            let mut out = vec![0u8; stride as usize * height as usize];
            converter
                .CopyPixels(std::ptr::null(), stride, &mut out)
                .code(FAILED)?;
            Ok((width, height, out))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_size_from_the_header() {
        let mut head = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        head.extend_from_slice(b"IHDR");
        head.extend_from_slice(&2560u32.to_be_bytes());
        head.extend_from_slice(&1440u32.to_be_bytes());
        assert_eq!(dimensions(&head), Some((2560, 1440)));
        assert_eq!(dimensions(&head[..20]), None);
        head[1] = b'J';
        assert_eq!(dimensions(&head), None);
    }

    /// Encode, read back with WIC: same pixels, and the header says the size.
    #[cfg(windows)]
    #[test]
    fn round_trips_through_wic() {
        let _com = crate::media::ComGuard::new();
        let (width, height) = (37u32, 5u32);
        let bgr: Vec<u8> = (0..width * height * 3)
            .map(|i| (i * 7 % 251) as u8)
            .collect();
        let png = encode_bgr(width, height, &bgr).expect("encode");
        assert_eq!(dimensions(&png), Some((width, height)));
        let (w, h, pixels) = decode_bgr(&png).expect("decode");
        assert_eq!((w, h), (width, height));
        assert_eq!(pixels, bgr, "lossless");
        assert!(
            encode_bgr(width, height, &bgr[1..]).is_err(),
            "too few pixels"
        );
    }
}

//! In-memory PNG/JPEG encoding and decoding through the Windows Imaging Component.
//! Streams are `SHCreateMemStream` buffers: nothing is ever written to disk.

use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatJpeg, GUID_ContainerFormatPng,
    GUID_WICPixelFormat24bppBGR, GUID_WICPixelFormat32bppBGRA, IWICBitmapFrameEncode,
    IWICBitmapSource, IWICImagingFactory, WICBitmapEncoderNoCache,
    WICBitmapInterpolationModeHighQualityCubic, WICConvertBitmapSource,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::StructuredStorage::{IPropertyBag2, PROPBAG2};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, IStream, STREAM_SEEK_END, STREAM_SEEK_SET,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::core::{GUID, PWSTR, w};
use winwright_contracts::capture::ImageFormat;
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::com::platform;
use crate::raster::{Bgra, jpeg_quality, raw_len};

pub fn container_format(format: ImageFormat) -> GUID {
    match format {
        ImageFormat::Png => GUID_ContainerFormatPng,
        ImageFormat::Jpeg => GUID_ContainerFormatJpeg,
    }
}

/// Pixel format requested from the encoder. Captures are opaque, so 24-bit BGR loses nothing
/// and keeps PNGs a quarter smaller than BGRA; JPEG has no alpha anyway.
const ENCODE_FORMAT: GUID = GUID_WICPixelFormat24bppBGR;

pub struct Wic {
    factory: IWICImagingFactory,
}

impl Wic {
    /// Requires COM on the calling thread.
    pub fn new() -> WinwrightResult<Self> {
        // SAFETY: plain in-proc COM activation on an initialized apartment.
        let factory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| WinwrightError::BackendUnavailable {
                    backend: "WIC".into(),
                    reason: format!("cannot create the imaging factory: {e}"),
                })?;
        Ok(Self { factory })
    }

    /// Encodes tightly packed BGRA pixels, scaled to `size` (width, height) when it differs.
    /// Alpha is ignored (captures are forced opaque).
    pub fn encode(
        &self,
        image: &Bgra,
        size: (u32, u32),
        format: ImageFormat,
        quality: u8,
    ) -> WinwrightResult<Vec<u8>> {
        let err = |op: &'static str| move |e: windows::core::Error| platform(op, &e);
        let stream = mem_stream(None)?;
        // SAFETY: every call below is a WIC/COM call on objects owned by this function; the
        // pixel slice outlives CreateBitmapFromMemory, which copies it.
        unsafe {
            let encoder = self
                .factory
                .CreateEncoder(&container_format(format), std::ptr::null())
                .map_err(err("IWICImagingFactory::CreateEncoder"))?;
            encoder
                .Initialize(&stream, WICBitmapEncoderNoCache)
                .map_err(err("IWICBitmapEncoder::Initialize"))?;
            let mut frame: Option<IWICBitmapFrameEncode> = None;
            let mut options: Option<IPropertyBag2> = None;
            encoder
                .CreateNewFrame(&mut frame, &mut options)
                .map_err(err("IWICBitmapEncoder::CreateNewFrame"))?;
            let frame = frame.ok_or_else(|| missing("IWICBitmapEncoder::CreateNewFrame"))?;
            if format == ImageFormat::Jpeg
                && let Some(bag) = &options
            {
                set_jpeg_quality(bag, quality)?;
            }
            frame
                .Initialize(options.as_ref())
                .map_err(err("IWICBitmapFrameEncode::Initialize"))?;
            frame
                .SetSize(size.0, size.1)
                .map_err(err("IWICBitmapFrameEncode::SetSize"))?;
            // The encoder may substitute the closest format it supports; convert to whatever
            // it chose rather than assuming.
            let mut chosen = ENCODE_FORMAT;
            frame
                .SetPixelFormat(&mut chosen)
                .map_err(err("IWICBitmapFrameEncode::SetPixelFormat"))?;
            let bitmap = self
                .factory
                .CreateBitmapFromMemory(
                    image.width,
                    image.height,
                    &GUID_WICPixelFormat32bppBGRA,
                    image.stride() as u32,
                    &image.pixels,
                )
                .map_err(err("IWICImagingFactory::CreateBitmapFromMemory"))?;
            let mut pixels: IWICBitmapSource = bitmap.into();
            if size != (image.width, image.height) {
                let scaler = self
                    .factory
                    .CreateBitmapScaler()
                    .map_err(err("IWICImagingFactory::CreateBitmapScaler"))?;
                scaler
                    .Initialize(
                        &pixels,
                        size.0,
                        size.1,
                        WICBitmapInterpolationModeHighQualityCubic,
                    )
                    .map_err(err("IWICBitmapScaler::Initialize"))?;
                pixels = scaler.into();
            }
            let source =
                WICConvertBitmapSource(&chosen, &pixels).map_err(err("WICConvertBitmapSource"))?;
            frame
                .WriteSource(&source, std::ptr::null())
                .map_err(err("IWICBitmapFrameEncode::WriteSource"))?;
            frame
                .Commit()
                .map_err(err("IWICBitmapFrameEncode::Commit"))?;
            encoder.Commit().map_err(err("IWICBitmapEncoder::Commit"))?;
        }
        read_all(&stream)
    }

    /// Decodes the first frame of a PNG/JPEG (or any WIC-supported image) into BGRA.
    pub fn decode(&self, bytes: &[u8]) -> WinwrightResult<Bgra> {
        let stream = mem_stream(Some(bytes))?;
        // SAFETY: WIC calls on objects owned by this function; `pixels` is sized for the full
        // image at the stride passed.
        unsafe {
            let decoder = self
                .factory
                .CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
                .map_err(|e| {
                    WinwrightError::invalid(format!(
                        "not a decodable image (HRESULT {:#010x})",
                        e.code().0
                    ))
                })?;
            let frame = decoder
                .GetFrame(0)
                .map_err(|e| platform("IWICBitmapDecoder::GetFrame", &e))?;
            let source = WICConvertBitmapSource(&GUID_WICPixelFormat32bppBGRA, &frame)
                .map_err(|e| platform("WICConvertBitmapSource", &e))?;
            let (mut width, mut height) = (0, 0);
            source
                .GetSize(&mut width, &mut height)
                .map_err(|e| platform("IWICBitmapSource::GetSize", &e))?;
            let mut pixels = vec![0; raw_len(width, height)?];
            source
                .CopyPixels(std::ptr::null(), width * 4, &mut pixels)
                .map_err(|e| platform("IWICBitmapSource::CopyPixels", &e))?;
            Ok(Bgra {
                width,
                height,
                pixels,
            })
        }
    }
}

fn missing(operation: &str) -> WinwrightError {
    WinwrightError::Platform {
        operation: operation.to_owned(),
        hresult: windows::Win32::Foundation::E_POINTER.0,
    }
}

/// Sets `ImageQuality` on the encoder-options bag from `CreateNewFrame`, before frame Initialize.
fn set_jpeg_quality(bag: &IPropertyBag2, quality: u8) -> WinwrightResult<()> {
    let option = PROPBAG2 {
        // WIC only reads the name; the cast drops `const` for the struct field's type.
        pstrName: PWSTR(w!("ImageQuality").0.cast_mut()),
        ..Default::default()
    };
    let value = VARIANT::from(jpeg_quality(quality));
    // SAFETY: one property/value pair, both alive for the call.
    unsafe { bag.Write(1, &option, &value) }.map_err(|e| platform("IPropertyBag2::Write", &e))
}

fn mem_stream(initial: Option<&[u8]>) -> WinwrightResult<IStream> {
    // SAFETY: SHCreateMemStream copies `initial`, so the stream owns its buffer.
    unsafe { SHCreateMemStream(initial) }.ok_or_else(|| missing("SHCreateMemStream"))
}

fn read_all(stream: &IStream) -> WinwrightResult<Vec<u8>> {
    let mut size = 0u64;
    // SAFETY: seek/read on a memory stream owned by the caller; the read target is a Vec
    // with exactly `size` initialized bytes.
    unsafe {
        stream
            .Seek(0, STREAM_SEEK_END, Some(&mut size))
            .map_err(|e| platform("IStream::Seek", &e))?;
        stream
            .Seek(0, STREAM_SEEK_SET, None)
            .map_err(|e| platform("IStream::Seek", &e))?;
        let len = u32::try_from(size).map_err(|_| WinwrightError::CaptureFailed {
            reason: format!("encoded image is too large ({size} bytes)"),
        })?;
        let mut bytes = vec![0u8; len as usize];
        let mut read = 0u32;
        stream
            .Read(bytes.as_mut_ptr().cast(), len, Some(&mut read))
            .ok()
            .map_err(|e| platform("IStream::Read", &e))?;
        bytes.truncate(read as usize);
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::com::ComApartment;

    #[test]
    fn format_maps_to_wic_container() {
        assert_eq!(container_format(ImageFormat::Png), GUID_ContainerFormatPng);
        assert_eq!(
            container_format(ImageFormat::Jpeg),
            GUID_ContainerFormatJpeg
        );
        assert_eq!(ImageFormat::default(), ImageFormat::Png);
    }

    fn checker(width: u32, height: u32) -> Bgra {
        let mut img = Bgra::black(width, height).unwrap();
        for y in 0..height {
            for x in 0..width {
                if (x / 4 + y / 4) % 2 == 0 {
                    let i = (y * width + x) as usize * 4;
                    img.pixels[i..i + 4].copy_from_slice(&[192, 128, 32, 255]);
                }
            }
        }
        img
    }

    #[test]
    fn png_round_trip_is_lossless() {
        let _com = ComApartment::ensure().unwrap();
        let wic = Wic::new().unwrap();
        let img = checker(37, 21);
        let png = wic
            .encode(&img, (img.width, img.height), ImageFormat::Png, 0)
            .unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let back = wic.decode(&png).unwrap();
        assert_eq!(back, img);
    }

    #[test]
    fn jpeg_quality_changes_size() {
        let _com = ComApartment::ensure().unwrap();
        let wic = Wic::new().unwrap();
        let img = checker(64, 64);
        let size = (img.width, img.height);
        let low = wic.encode(&img, size, ImageFormat::Jpeg, 5).unwrap();
        let high = wic.encode(&img, size, ImageFormat::Jpeg, 100).unwrap();
        assert_eq!(&low[..3], &[0xFF, 0xD8, 0xFF]);
        assert!(low.len() < high.len(), "{} !< {}", low.len(), high.len());
        let back = wic.decode(&high).unwrap();
        assert_eq!((back.width, back.height), (64, 64));
        assert!(back.pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
    }

    #[test]
    fn garbage_is_rejected() {
        let _com = ComApartment::ensure().unwrap();
        let err = Wic::new().unwrap().decode(b"not an image").unwrap_err();
        assert_eq!(err.code().as_str(), "INVALID_REQUEST");
    }
}

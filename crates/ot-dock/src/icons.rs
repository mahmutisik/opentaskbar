//! Application icons from the Shell (`ApplicationIconProvider.GetForShellItem` in OpenTaskbar): any parsing name
//! (an .exe, `shell:AppsFolder\<AUMID>`, a CLSID like the Recycle Bin) at a requested pixel size, as premultiplied
//! BGRA rows ready for Direct2D. Extraction runs off the UI thread; results are cached per (name, size).

use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, GetDIBits, GetObjectW, HBITMAP, HGDIOBJ,
};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
    SIIGBF_SCALEUP,
};
use windows::Win32::UI::WindowsAndMessaging::{GetIconInfo, HICON, ICONINFO};
use windows::core::HSTRING;

/// A premultiplied BGRA image.
#[derive(Clone, Debug)]
pub struct Icon {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Loads an icon for a Shell parsing name. COM must be initialized on the calling thread.
pub fn load(parsing_name: &str, size: u32) -> Option<Icon> {
    // SAFETY: COM calls; the HBITMAP is deleted after its pixels are copied.
    unsafe {
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(&HSTRING::from(parsing_name), None).ok()?;
        let bitmap = factory
            .GetImage(
                SIZE {
                    cx: size as i32,
                    cy: size as i32,
                },
                SIIGBF_BIGGERSIZEOK | SIIGBF_ICONONLY | SIIGBF_SCALEUP,
            )
            .ok()?;
        from_hbitmap(bitmap)
    }
}

/// Copies a 32-bit DIB section's pixels (premultiplied) and deletes the bitmap.
/// Decodes an image file (PNG and the other WIC formats) to premultiplied BGRA, optionally scaled to `size` with
/// the Fant filter (WPF's high-quality scaling). COM must be initialized on the calling thread.
pub fn decode_image(bytes: &[u8], size: Option<(u32, u32)>) -> Option<Icon> {
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmapSource,
        IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant,
        WICBitmapPaletteTypeMedianCut, WICDecodeMetadataCacheOnLoad,
    };
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    use windows::core::Interface;
    if bytes.is_empty() {
        return None;
    }
    // SAFETY: WIC calls with valid arguments; `bytes` outlives the stream.
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let stream = factory.CreateStream().ok()?;
        stream.InitializeFromMemory(bytes).ok()?;
        let decoder = factory
            .CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnLoad)
            .ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let (mut width, mut height) = (0u32, 0u32);
        frame.GetSize(&mut width, &mut height).ok()?;
        let mut source: IWICBitmapSource = frame.cast().ok()?;
        if let Some((w, h)) = size
            && (w, h) != (width, height)
            && w > 0
            && h > 0
        {
            let scaler = factory.CreateBitmapScaler().ok()?;
            scaler
                .Initialize(&source, w, h, WICBitmapInterpolationModeFant)
                .ok()?;
            source = scaler.cast().ok()?;
            width = w;
            height = h;
        }
        let converter = factory.CreateFormatConverter().ok()?;
        converter
            .Initialize(
                &source,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeMedianCut,
            )
            .ok()?;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        converter
            .CopyPixels(std::ptr::null(), width * 4, &mut pixels)
            .ok()?;
        Some(Icon {
            width,
            height,
            pixels,
        })
    }
}

pub fn from_hbitmap(bitmap: HBITMAP) -> Option<Icon> {
    // SAFETY: the bitmap is a DIB section from the Shell; it is deleted after its pixels are copied.
    unsafe {
        let mut info = BITMAP::default();
        let read = GetObjectW(
            HGDIOBJ(bitmap.0),
            size_of::<BITMAP>() as i32,
            Some((&mut info as *mut BITMAP).cast()),
        );
        let icon = if read != 0 && !info.bmBits.is_null() && info.bmBitsPixel == 32 {
            let width = info.bmWidth as usize;
            let height = info.bmHeight.unsigned_abs() as usize;
            let stride = info.bmWidthBytes as usize;
            let source = std::slice::from_raw_parts(info.bmBits as *const u8, stride * height);
            let mut pixels = vec![0u8; width * height * 4];
            for row in 0..height {
                // DIB sections with a positive height are stored bottom-up.
                let source_row = if info.bmHeight > 0 {
                    height - 1 - row
                } else {
                    row
                };
                pixels[row * width * 4..(row + 1) * width * 4]
                    .copy_from_slice(&source[source_row * stride..source_row * stride + width * 4]);
            }
            ensure_premultiplied(&mut pixels);
            Some(Icon {
                width: width as u32,
                height: height as u32,
                pixels,
            })
        } else {
            None
        };
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        icon
    }
}

/// The Shell parsing name for an app: its packaged AUMID when known (Store apps cannot be read from WindowsApps),
/// otherwise the executable.
pub fn parsing_name(executable_path: &str, app_user_model_id: Option<&str>) -> String {
    match app_user_model_id {
        Some(aumid) if aumid.contains('!') => format!(r"shell:AppsFolder\{aumid}"),
        _ => executable_path.to_owned(),
    }
}

/// The Recycle Bin's CLSID parsing name; its icon shows empty or full.
pub const RECYCLE_BIN: &str = "::{645FF040-5081-101B-9F08-00AA002F954E}";

/// Shell bitmaps are usually premultiplied, but some icons (e.g. browser web-app .ico files) come back with straight
/// alpha, which shows as a bright fringe. A color channel above its alpha is impossible in premultiplied data, so
/// that proves straight alpha; the image is then premultiplied.
pub fn ensure_premultiplied(bgra: &mut [u8]) {
    let straight = bgra
        .as_chunks::<4>()
        .0
        .iter()
        .any(|p| p[0] > p[3] || p[1] > p[3] || p[2] > p[3]);
    if !straight {
        return;
    }
    for pixel in bgra.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = ((u32::from(*channel) * alpha + 127) / 255) as u8;
        }
    }
}

/// The BGRA pixels of an icon: its color bitmap, with the mask as alpha for icons that have none.
pub fn from_hicon(icon: HICON) -> Option<Icon> {
    // SAFETY: GDI reads into buffers sized from the bitmaps; the bitmaps from GetIconInfo are deleted.
    unsafe {
        let mut info = ICONINFO::default();
        GetIconInfo(icon, &mut info).ok()?;
        let result = (|| {
            if info.hbmColor.is_invalid() {
                return None;
            }
            let mut bitmap = BITMAP::default();
            if GetObjectW(
                HGDIOBJ(info.hbmColor.0),
                size_of::<BITMAP>() as i32,
                Some((&mut bitmap as *mut BITMAP).cast()),
            ) == 0
            {
                return None;
            }
            let (width, height) = (bitmap.bmWidth, bitmap.bmHeight.abs());
            let color = dib_rows(info.hbmColor, width, height)?;
            let mut pixels = color;
            if pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
                let mask = dib_rows(info.hbmMask, width, height)?;
                for (pixel, m) in pixels
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(mask.as_chunks::<4>().0)
                {
                    pixel[3] = if m[0] == 0 { 255 } else { 0 };
                }
            }
            ensure_premultiplied(&mut pixels);
            Some(Icon {
                width: width as u32,
                height: height as u32,
                pixels,
            })
        })();
        if !info.hbmColor.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
        }
        if !info.hbmMask.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
        }
        result
    }
}

/// A bitmap's pixels as top-down 32-bit BGRA rows.
fn dib_rows(bitmap: HBITMAP, width: i32, height: i32) -> Option<Vec<u8>> {
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: a memory DC for GetDIBits, deleted below; the buffer holds `height` rows of `width` pixels.
    unsafe {
        let dc = CreateCompatibleDC(None);
        let lines = GetDIBits(
            dc,
            bitmap,
            0,
            height as u32,
            Some(pixels.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        );
        let _ = DeleteDC(dc);
        (lines == height).then_some(pixels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn straight_alpha_is_premultiplied_once() {
        let mut straight = vec![255, 0, 0, 128, 10, 10, 10, 255];
        ensure_premultiplied(&mut straight);
        assert_eq!(straight, vec![128, 0, 0, 128, 10, 10, 10, 255]);
        let mut premultiplied = vec![100, 0, 0, 128];
        ensure_premultiplied(&mut premultiplied);
        assert_eq!(
            premultiplied,
            vec![100, 0, 0, 128],
            "already valid data is untouched"
        );
    }
}

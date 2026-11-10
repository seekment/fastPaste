//! FR-Q03: local OCR for image clipboard entries.
//! - Windows: Windows.Media.Ocr (WinRT) - enabled with feature `ocr-win`.
//! - macOS:   Vision VNRecognizeTextRequest (.accurate).
//! Default build ships a no-op engine so the binary stays lean (NFR-P01).
pub trait OcrEngine: Send + Sync {
    fn recognize(&self, image: &[u8]) -> Option<String>;
}

struct Noop;
impl OcrEngine for Noop {
    fn recognize(&self, _image: &[u8]) -> Option<String> { None }
}

pub fn recognize(image: &[u8]) -> Option<String> {
    engine().recognize(image)
}

fn engine() -> &'static dyn OcrEngine {
    use once_cell::sync::OnceCell;
    static E: OnceCell<&'static dyn OcrEngine> = OnceCell::new();
    *E.get_or_init(|| {
        #[cfg(all(target_os = "macos", feature = "ocr-mac"))]
        { &MacVision }
        #[cfg(all(windows, feature = "ocr-win"))]
        { &WinOcr }
        #[cfg(not(any(all(target_os = "macos", feature = "ocr-mac"), all(windows, feature = "ocr-win"))))]
        { &Noop }
    })
}

#[cfg(all(target_os = "macos", feature = "ocr-mac"))]
mod vision {
    use super::OcrEngine;
    use block::ConcreteBlock;
    use objc::{class, msg_send, sel, sel_impl};
    use objc::runtime::Object;
    use std::sync::{Arc, Mutex};

    pub struct MacVision;
    impl OcrEngine for MacVision {
        fn recognize(&self, image: &[u8]) -> Option<String> {
            unsafe { recognize_impl(image) }
        }
    }

    unsafe fn recognize_impl(image: &[u8]) -> Option<String> {
        use core_graphics::base::kCGRenderingIntentDefault;
        use core_graphics::color_space::CGColorSpace;
        use core_graphics::data_provider::CGDataProvider;
        use core_graphics::image::CGImage;

        // decode PNG/BMP via `image` crate, convert to RGBA8 for CGImage
        let fmt = if image.starts_with(b"BM") { image::ImageFormat::Bmp } else { image::ImageFormat::Png };
        let dynimg = image::load_from_memory_with_format(image, fmt).ok()?;
        let rgba = dynimg.to_rgba8();
        let (w, h) = rgba.dimensions();
        let provider = CGDataProvider::from_buffer(Arc::new(rgba.into_raw().into_boxed_slice()));
        let cs = CGColorSpace::create_device_rgb();
        let cgimg = CGImage::new(w as usize, h as usize, 8, 32, w as usize * 4, &cs,
            core_graphics::bitmap_info::CGBitmapInfo::BYTE_ORDER_DEFAULT
                | core_graphics::bitmap_info::kCGBitmapAlphaInfoMask
                | core_graphics::bitmap_info::kCGImageAlphaPremultipliedLast,
            &provider, false, kCGRenderingIntentDefault);

        let results: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
        let results2 = results.clone();
        let block = ConcreteBlock::new(move |request: *mut Object, _err: *mut Object| {
            if request.is_null() { return; }
            let observations: *mut Object = msg_send![request, results];
            let count: usize = msg_send![observations, count];
            let mut out = results2.lock().unwrap();
            for i in 0..count {
                let obs: *mut Object = msg_send![observations, objectAtIndex: i];
                let candidates: *mut Object = msg_send![obs, topCandidates: 1usize];
                if candidates.is_null() { continue; }
                let c: usize = msg_send![candidates, count];
                if c == 0 { continue; }
                let cand: *mut Object = msg_send![candidates, objectAtIndex: 0usize];
                let s: *mut Object = msg_send![cand, string];
                if !s.is_null() {
                    let utf8: *const std::ffi::c_char = msg_send![s, UTF8String];
                    if !utf8.is_null() {
                        out.push(std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned());
                    }
                }
            }
        });
        let block = block.copy();

        let request: *mut Object = msg_send![class!(VNRecognizeTextRequest), alloc];
        let request: *mut Object = msg_send![request, initWithCompletionHandler: block];
        if request.is_null() { return None; }
        let _: () = msg_send![request, setRecognitionLevel: 0usize]; // VNRequestTextRecognitionLevelAccurate
        let handler: *mut Object = msg_send![class!(VNImageRequestHandler), alloc];
        let cg = &*cgimg as *const _ as *mut Object;
        let handler: *mut Object = msg_send![handler, initWithCGImage: cg options: std::ptr::null_mut::<Object>()];
        if handler.is_null() { return None; }
        let req: *mut Object = request;
        let arr: *mut Object = msg_send![class!(NSArray), arrayWithObject: req];
        let ok: bool = msg_send![handler, performRequests: arr error: std::ptr::null_mut::<*mut Object>()];
        if !ok { return None; }
        let parts = results.lock().unwrap().clone();
        if parts.is_empty() { None } else { Some(parts.join("\n")) }
    }
}
#[cfg(all(target_os = "macos", feature = "ocr-mac"))]
pub use vision::MacVision;

#[cfg(all(windows, feature = "ocr-win"))]
mod win_ocr {
    use super::OcrEngine;
    /// Windows.Media.Ocr via the `windows` crate (WinRT). Language pack from OS.
    pub struct WinOcr;
    impl OcrEngine for WinOcr {
        fn recognize(&self, image: &[u8]) -> Option<String> {
            win_ocr_impl(image)
        }
    }
    fn win_ocr_impl(image: &[u8]) -> Option<String> {
        // Decode with `image`, encode as PNG in-memory, feed into WinRT decoder.
        let fmt = if image.starts_with(b"BM") { image::ImageFormat::Bmp } else { image::ImageFormat::Png };
        let dynimg = image::load_from_memory_with_format(image, fmt).ok()?;
        let mut png = std::io::Cursor::new(Vec::new());
        dynimg.write_to(&mut png, image::ImageFormat::Png).ok()?;
        windows_impl(&png.into_inner())
    }
    #[cfg(feature = "ocr-win")]
    fn windows_impl(png: &[u8]) -> Option<String> {
        use windows::Foundation::IAsyncOperation;
        use windows::Graphics::Imaging::{BitmapDecoder, BitmapPixelFormat, SoftwareBitmap};
        use windows::Media::Ocr::OcrEngine;
        use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};

        // 1) PNG bytes -> IRandomAccessStream
        let stream = InMemoryRandomAccessStream::new().ok()?;
        {
            let writer = DataWriter::CreateDataWriter(&stream).ok()?;
            writer.WriteBytes(png).ok()?;
            writer.StoreAsync().ok()?.get().ok()?;
            writer.FlushAsync().ok()?.get().ok()?;
            writer.DetachStream().ok()?;
        }
        stream.Seek(0).ok()?;

        // 2) decode -> SoftwareBitmap (Gray8 has the widest OCR engine support)
        let decoder = BitmapDecoder::CreateAsync(&stream).ok()?.get().ok()?;
        let bitmap = decoder.GetSoftwareBitmapAsync().ok()?.get().ok()?;
        let bitmap = SoftwareBitmap::Convert(&bitmap, BitmapPixelFormat::Gray8, None)
            .unwrap_or(bitmap);

        // 3) OCR with the user's profile languages (系统已安装的语言包, 6.3)
        let engine = OcrEngine::TryCreateFromUserProfileLanguages().ok()?;
        let result = engine.RecognizeAsync(&bitmap).ok()?.get().ok()?;
        let text = result.Text().ok()?;
        let text = text.to_string();
        if text.trim().is_empty() { None } else { Some(text) }
    }
}
#[cfg(all(windows, feature = "ocr-win"))]
pub use win_ocr::WinOcr;

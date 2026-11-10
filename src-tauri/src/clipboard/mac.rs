//! macOS clipboard: NSPasteboard.changeCount polling (FR-S08), 500 ms default,
//! 1000 ms in low-power mode. FR-S02: canReadObject -> read ONE class.
//! FR-S04: org.nspasteboard.TransientType. FR-S09: paste-aware mode defers reads.
use super::RawClip;
use objc::{class, msg_send, sel, sel_impl};
use objc::runtime::Object;
use objc_foundation::{INSArray, INSString, NSArray, NSObject, NSString};
use std::sync::mpsc::Sender;

const POLL_MS: u64 = 500;
const POLL_LOW_POWER_MS: u64 = 1000;

extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

/// FR-K / 3.4: Accessibility (辅助功能) permission state.
pub fn accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

unsafe fn general_pasteboard() -> *mut Object {
    msg_send![class!(NSPasteboard), generalPasteboard]
}

unsafe fn change_count() -> i64 {
    msg_send![general_pasteboard(), changeCount]
}

unsafe fn low_power_mode() -> bool {
    let pi: *mut Object = msg_send![class!(NSProcessInfo), processInfo];
    let b: bool = msg_send![pi, isLowPowerModeEnabled];
    b
}

unsafe fn has_type(pb: *mut Object, name: &str) -> bool {
    let t = NSString::from_str(name);
    let b: bool = msg_send![pb, hasType: t];
    b
}

fn nsstring_to_string(s: *mut NSString) -> String {
    if s.is_null() { return String::new(); }
    unsafe { (*s).as_str().to_string() }
}

/// Read NSString array (first object) from pasteboard for a given class probe.
unsafe fn read_objects_for_classes(classes: &[&'static objc::runtime::Class]) -> Option<(*mut NSArray<NSObject>, usize)> {
    let pb = general_pasteboard();
    let arr: *mut NSArray<NSObject> = msg_send![class!(NSArray), alloc];
    let arr: *mut NSArray<NSObject> = msg_send![arr, initWithObjects: classes.as_ptr() count: 1usize];
    let can: bool = msg_send![pb, canReadObjectForClasses: arr options: std::ptr::null_mut::<Object>()];
    if !can { return None; }
    let out: *mut NSArray<NSObject> = msg_send![pb, readObjectsForClasses: arr options: std::ptr::null_mut::<Object>()];
    if out.is_null() { return None; }
    let count: usize = msg_send![out, count];
    if count == 0 { return None; }
    Some((out, count))
}

/// FR-S08: polling thread. Signals tx whenever changeCount changes.
pub fn spawn_listener(tx: Sender<()>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("fp-clip-listener".into())
        .spawn(move || {
            let mut last = unsafe { change_count() };
            loop {
                let interval = if unsafe { low_power_mode() } { POLL_LOW_POWER_MS } else { POLL_MS };
                std::thread::sleep(std::time::Duration::from_millis(interval));
                let cur = unsafe { change_count() };
                if cur != last {
                    last = cur;
                    if tx.send(()).is_err() { break; }
                }
            }
        })
        .expect("spawn listener")
}

unsafe fn app_name_of_frontmost() -> String {
    // best-effort source attribution via frontmost app (NSWorkspace)
    let ws: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
    let app: *mut Object = msg_send![ws, frontmostApplication];
    if app.is_null() { return String::new(); }
    let name: *mut NSString = msg_send![app, localizedName];
    nsstring_to_string(name as *mut NSString)
}

/// FR-S02 probe -> one class -> read. FR-S04 transient filter.
pub fn read_primary(app_override: Option<&str>) -> Option<(RawClip, String)> {
    unsafe {
        let pb = general_pasteboard();
        if has_type(pb, "org.nspasteboard.TransientType") { return None; }
        if has_type(pb, "org.nspasteboard.ConcealedType") { return None; }

        // Files > Image > HTML > RTF > Text (FR-S02 priority)
        let url_class = class!(NSURL);
        if let Some((arr, _)) = read_objects_for_classes(&[&url_class]) {
            let obj: *mut Object = msg_send![arr, objectAtIndex: 0usize];
            let is_file: bool = msg_send![obj, isFileURL];
            if is_file {
                let mut files = vec![];
                let count: usize = msg_send![arr, count];
                for i in 0..count {
                    let o: *mut Object = msg_send![arr, objectAtIndex: i];
                    let p: *mut NSString = msg_send![o, path];
                    files.push(nsstring_to_string(p as *mut NSString));
                }
                let app = app_override.map(str::to_string).unwrap_or_else(|| app_name_of_frontmost());
                return Some((RawClip::Files(files), app));
            }
        }
        let img_class = class!(NSImage);
        if let Some((arr, _)) = read_objects_for_classes(&[&img_class]) {
            let img: *mut Object = msg_send![arr, objectAtIndex: 0usize];
            let tiff: *mut Object = msg_send![img, TIFFRepresentation];
            if !tiff.is_null() {
                let bytes: *mut u8 = msg_send![tiff, bytes];
                let len: usize = msg_send![tiff, length];
                let data = std::slice::from_raw_parts(bytes, len).to_vec();
                // store as PNG for cross-tool friendliness
                if let Ok(dynimg) = image::load_from_memory_with_format(&data, image::ImageFormat::Tiff) {
                    let mut png = std::io::Cursor::new(Vec::new());
                    if dynimg.write_to(&mut png, image::ImageFormat::Png).is_ok() {
                        let app = app_override.map(str::to_string).unwrap_or_else(|| app_name_of_frontmost());
                        return Some((RawClip::Image { bytes: png.into_inner(), bmp_dib: false }, app));
                    }
                }
            }
        }
        if has_type(pb, "public.html") {
            let cls = class!(NSAttributedString);
            let _ = cls;
            let html_str = NSString::from_str("public.html");
            let data: *mut Object = msg_send![pb, dataForType: html_str];
            if !data.is_null() {
                let bytes: *mut u8 = msg_send![data, bytes];
                let len: usize = msg_send![data, length];
                let s = String::from_utf8_lossy(std::slice::from_raw_parts(bytes, len)).to_string();
                let app = app_override.map(str::to_string).unwrap_or_else(|| app_name_of_frontmost());
                return Some((RawClip::Html(s), app));
            }
        }
        let str_class = class!(NSString);
        if let Some((arr, _)) = read_objects_for_classes(&[&str_class]) {
            let s: *mut NSString = msg_send![arr, objectAtIndex: 0usize];
            let text = nsstring_to_string(s);
            let app = app_override.map(str::to_string).unwrap_or_else(|| app_name_of_frontmost());
            return Some((RawClip::Text(text), app));
        }
        None
    }
}

/// FR-S05: write plain text / HTML, no private types.
pub fn write_clipboard(text: Option<&str>, html: Option<&str>) -> bool {
    unsafe {
        let pb = general_pasteboard();
        let _: i64 = msg_send![pb, clearContents];
        if let Some(t) = text {
            let s = NSString::from_str(t);
            let ok: bool = msg_send![pb, setString: s];
            if !ok { return false; }
        }
        if let Some(h) = html {
            let ty = NSString::from_str("public.html");
            let data: *mut Object = msg_send![class!(NSData), alloc];
            let bytes = h.as_ptr() as *const std::ffi::c_void;
            let data: *mut Object = msg_send![data, initWithBytes: bytes length: h.len()];
            let ok: bool = msg_send![pb, setData: data forType: ty];
            if !ok { return false; }
        }
        true
    }
}

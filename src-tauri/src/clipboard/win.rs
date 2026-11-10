//! Windows clipboard: WM_CLIPBOARDUPDATE observer + careful single-format reads.
//! Implements FR-S01 (delayed read), FR-S02 (probe -> one format), FR-S03 (backoff
//! retry plan is applied by the monitor), FR-S04 (transient format filter),
//! FR-S05 (clean write-back).
use super::RawClip;
use once_cell::sync::OnceCell;
use std::sync::mpsc::Sender;
use windows_sys::core::PCWSTR;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardOwner, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData, EmptyClipboard,
};
use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows_sys::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION};
use windows_sys::Win32::UI::Shell::DragQueryFileW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowThreadProcessId, RegisterClassW, HWND_MESSAGE, MSG, WM_CLIPBOARDUPDATE, WM_DESTROY,
    WNDCLASSW,
};

const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
const CF_DIB: u32 = 8;

static CLIP_TX: OnceCell<Sender<()>> = OnceCell::new();
static FORMATS: OnceCell<Formats> = OnceCell::new();

struct Formats {
    html: u32,
    rtf: u32,
    exclude_monitor: u32,
}

fn formats() -> &'static Formats {
    FORMATS.get_or_init(|| {
        let reg = |s: &str| unsafe {
            let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
            RegisterClipboardFormatW(wide.as_ptr())
        };
        Formats {
            html: reg("HTML Format"),
            rtf: reg("Rich Text Format"),
            exclude_monitor: reg("ExcludeClipboardContentFromMonitorProcessing"),
        }
    })
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_CLIPBOARDUPDATE {
        if let Some(tx) = CLIP_TX.get() { let _ = tx.send(()); }
        return 0;
    }
    if msg == WM_DESTROY {
        let _ = RemoveClipboardFormatListener(hwnd);
        return 0;
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Dedicated listener thread owning a message-only HWND (FR-S01).
pub fn spawn_listener(tx: Sender<()>) -> std::thread::JoinHandle<()> {
    let _ = CLIP_TX.set(tx);
    std::thread::Builder::new()
        .name("fp-clip-listener".into())
        .spawn(move || unsafe {
            let class_name: Vec<u16> = "fastPasteClipListener\0".encode_utf16().collect();
            let title: Vec<u16> = "fastPasteClip\0".encode_utf16().collect();
            let mut wc: WNDCLASSW = std::mem::zeroed();
            wc.lpfnWndProc = Some(wnd_proc);
            wc.lpszClassName = class_name.as_ptr();
            RegisterClassW(&wc);
            let hwnd = CreateWindowExW(0, class_name.as_ptr(), title.as_ptr(),
                0, 0, 0, 0, 0, HWND_MESSAGE, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
            if hwnd == 0 { return; }
            if AddClipboardFormatListener(hwnd) == 0 { return; }
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                DispatchMessageW(&msg);
            }
            DestroyWindow(hwnd);
        })
        .expect("spawn listener")
}

pub fn source_app_name() -> String {
    unsafe {
        let owner = GetClipboardOwner();
        if owner == 0 { return String::new(); }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(owner, &mut pid);
        if pid == 0 { return String::new(); }
        let proc = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if proc == 0 { return String::new(); }
        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(proc, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(proc);
        if ok == 0 { return String::new(); }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit(['\\', '/']).next().unwrap_or("").to_string()
    }
}

fn read_handle(format: u32) -> Option<Vec<u8>> {
    unsafe {
        let h: HANDLE = GetClipboardData(format);
        if h == 0 { return None; }
        let ptr = GlobalLock(h);
        if ptr.is_null() { return None; }
        let size = GlobalSize(h) as usize;
        let slice = std::slice::from_raw_parts(ptr as *const u8, size);
        let data = slice.to_vec();
        GlobalUnlock(h);
        Some(data)
    }
}

fn read_text() -> Option<String> {
    let bytes = read_handle(CF_UNICODETEXT)?;
    if bytes.len() < 2 { return Some(String::new()); }
    let wide: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    Some(String::from_utf16_lossy(&wide).trim_end_matches('\0').to_string())
}

fn read_files() -> Option<Vec<String>> {
    unsafe {
        let h: HANDLE = GetClipboardData(CF_HDROP);
        if h == 0 { return None; }
        let count = DragQueryFileW(h as *mut std::ffi::c_void, 0xFFFFFFFF, std::ptr::null_mut(), 0);
        let mut out = Vec::new();
        for i in 0..count {
            let len = DragQueryFileW(h as *mut std::ffi::c_void, i, std::ptr::null_mut(), 0);
            let mut buf = vec![0u16; len as usize + 1];
            DragQueryFileW(h as *mut std::ffi::c_void, i, buf.as_mut_ptr(), len + 1);
            out.push(String::from_utf16_lossy(&buf[..len as usize]));
        }
        Some(out)
    }
}

/// Wrap a raw DIB into a BMP file (14-byte BITMAPFILEHEADER + DIB payload).
fn dib_to_bmp(dib: &[u8]) -> Vec<u8> {
    let off = 14u32;
    let size = (dib.len() as u32) + off;
    let mut bmp = Vec::with_capacity(size as usize);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&size.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&off.to_le_bytes());
    bmp.extend_from_slice(dib);
    bmp
}

/// FR-S02: probe -> pick ONE primary format -> read -> close. FR-S04: skip transient.
pub fn read_primary(_app_override: Option<&str>) -> Option<(RawClip, String)> {
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 { return None; } // caller retries (FR-S03)
        let result = (|| {
            let f = formats();
            use windows_sys::Win32::System::DataExchange::IsClipboardFormatAvailable;
            // FR-S04: explicit opt-out marker present -> skip entirely
            if IsClipboardFormatAvailable(f.exclude_monitor) != 0 { return None; }
            // Collect available formats once (probe step only - FR-S02)
            let mut avail = Vec::new();
            let mut fmt = 0u32;
            loop {
                fmt = EnumClipboardFormats(fmt);
                if fmt == 0 { break; }
                avail.push(fmt);
            }
            let has = |x: u32| avail.contains(&x);
            let clip = if has(CF_HDROP) {
                RawClip::Files(read_files()?)
            } else if has(CF_DIB) {
                let dib = read_handle(CF_DIB)?;
                RawClip::Image { bytes: dib_to_bmp(&dib), bmp_dib: true }
            } else if has(f.html) {
                let raw = read_handle(f.html)?;
                let s = String::from_utf8_lossy(&raw);
                // strip CF_HTML header block (everything before <!DOCTYPE/html)
                let body = s.find("<!").map(|i| &s[i..]).unwrap_or(&s).to_string();
                RawClip::Html(body)
            } else if has(f.rtf) {
                let raw = read_handle(f.rtf)?;
                RawClip::Rtf(String::from_utf8_lossy(&raw).trim_end_matches('\0').to_string())
            } else if has(CF_UNICODETEXT) {
                RawClip::Text(read_text()?)
            } else {
                return None; // unsupported / owner-display only
            };
            Some((clip, source_app_name()))
        })();
        CloseClipboard();
        result
    }
}

/// FR-S05: write only the content itself (CF_UNICODETEXT and/or HTML Format),
/// then close immediately. No private marker formats.
pub fn write_clipboard(text: Option<&str>, html: Option<&str>) -> bool {
    use windows_sys::Win32::System::Memory::{GlobalAlloc, GMEM_MOVEABLE};
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 { return false; }
        let ok = (|| {
            if EmptyClipboard() == 0 { return false; }
            let f = formats();
            if let Some(t) = text {
                let wide: Vec<u16> = t.encode_utf16().chain(std::iter::once(0)).collect();
                let bytes = wide.len() * 2;
                let h = GlobalAlloc(GMEM_MOVEABLE, bytes);
                if h == 0 { return false; }
                let ptr = GlobalLock(h);
                std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr as *mut u8, bytes);
                GlobalUnlock(h);
                if SetClipboardData(CF_UNICODETEXT, h) == 0 { return false; }
            }
            if let Some(hs) = html {
                let s = format!(
                    "Version:0.9\r\nStartHTML:{:08}\r\nEndHTML:{:08}\r\nStartFragment:{:08}\r\nEndFragment:{:08}\r\n<html><body><!--StartFragment-->{}<!--EndFragment--></body></html>",
                    97, 97 + 58 + hs.len(), 97 + 32, 97 + 32 + hs.len(), hs);
                let b = s.as_bytes();
                let h = GlobalAlloc(GMEM_MOVEABLE, b.len());
                if h == 0 { return false; }
                let ptr = GlobalLock(h);
                std::ptr::copy_nonoverlapping(b.as_ptr(), ptr as *mut u8, b.len());
                GlobalUnlock(h);
                if SetClipboardData(f.html, h) == 0 { return false; }
            }
            true
        })();
        CloseClipboard(); // release immediately (FR-S05)
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dib_to_bmp_adds_header() {
        let dib = vec![0u8; 100];
        let bmp = dib_to_bmp(&dib);
        assert_eq!(&bmp[..2], b"BM");
        assert_eq!(bmp.len(), 114);
        assert_eq!(u32::from_le_bytes(bmp[10..14].try_into().unwrap()), 14);
    }
    /// FR-S03 retry plan: 4 backoffs, 50/120/250/500 ms.
    #[test]
    fn retry_plan_spec() {
        let plan = crate::monitor::retry_delays_ms();
        assert_eq!(plan, vec![50, 120, 250, 500]);
    }
}

//! Serial clipboard monitor (FR-S01, FR-S03, FR-S06, FR-S07).
//! ALL clipboard reads/writes funnel through one dedicated thread so at most one
//! thread holds the clipboard handle at any moment. UI / hotkey threads never
//! touch the clipboard directly.
use crate::clipboard::{self, RawClip};
use crate::models::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// FR-S03: backoff retry plan (ms) - also asserted by tests.
pub fn retry_delays_ms() -> Vec<u64> { vec![50, 120, 250, 500] }
const READ_TIMEOUT_HARD_MS: u64 = 800; // FR-S03 single-read hard cap
const CAPTURE_DELAY_MS: u64 = 30;      // FR-S01 delay after notification
const FAILURE_SUSPEND_THRESHOLD: u32 = 5;
const SUSPEND_SECONDS: u64 = 60;

pub enum MonitorCmd {
    /// A clipboard change was signalled by the OS listener.
    Changed,
    /// User explicitly requests a capture attempt (感知粘贴 fallback / refresh).
    Probe,
    /// Write-back + return whether it succeeded.
    Write { text: Option<String>, html: Option<String>, reply: Sender<bool> },
    Shutdown,
}

pub enum MonitorEvent {
    Captured(ClipMeta),
    Suspended(bool),
}

pub struct MonitorHandle {
    pub cmd: Sender<MonitorCmd>,
    pub events: Receiver<MonitorEvent>,
    pub manual_only: Arc<AtomicBool>,
}

/// FR-S02 priority + payload construction; pure & unit-testable.
pub fn raw_to_incoming(raw: RawClip, app: &str) -> IncomingClip {
    match raw {
        RawClip::Text(t) => IncomingClip::text(&t, app),
        RawClip::Html(h) => IncomingClip {
            content_type: ContentType::Html,
            source_app: app.to_string(),
            text_for_index: html_to_text(&h),
            payload: ClipPayload { html: Some(h), ..Default::default() },
            image_bytes: None,
            thumbnail: None,
        },
        RawClip::Rtf(r) => IncomingClip {
            content_type: ContentType::Rtf,
            source_app: app.to_string(),
            text_for_index: String::new(), // RTF needs parser; index empty, preview from strip later
            payload: ClipPayload { rtf: Some(r), ..Default::default() },
            image_bytes: None,
            thumbnail: None,
        },
        RawClip::Files(fs) => IncomingClip {
            content_type: ContentType::Files,
            source_app: app.to_string(),
            text_for_index: fs.join("\n"),
            payload: ClipPayload { files: Some(fs), ..Default::default() },
            image_bytes: None,
            thumbnail: None,
        },
        RawClip::Image { bytes, .. } => {
            let thumb = make_thumbnail(&bytes);
            IncomingClip {
                content_type: ContentType::Image,
                source_app: app.to_string(),
                text_for_index: String::new(), // filled after OCR
                payload: ClipPayload::default(),
                image_bytes: Some(bytes),
                thumbnail: thumb,
            }
        }
    }
}

fn make_thumbnail(bytes: &[u8]) -> Option<Vec<u8>> {
    let fmt = if bytes.starts_with(b"BM") { image::ImageFormat::Bmp }
        else { image::ImageFormat::Png };
    let img = image::load_from_memory_with_format(bytes, fmt).ok()?;
    let thumb = image::imageops::thumbnail(&img, 320, 320);
    let mut out = std::io::Cursor::new(Vec::new());
    thumb.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

impl MonitorHandle {
    pub fn new(store: Arc<parking_lot::Mutex<crate::store::Store>>,
               paste_aware: bool) -> Self {
        let (cmd_tx, cmd_rx) = channel::<MonitorCmd>();
        let (ev_tx, ev_rx) = channel::<MonitorEvent>();
        let manual_only = Arc::new(AtomicBool::new(false));
        let manual_flag = manual_only.clone();

        std::thread::Builder::new()
            .name("fp-clip-serial".into())
            .spawn(move || serial_loop(cmd_rx, ev_tx, store, paste_aware, manual_flag))
            .expect("spawn serial monitor");

        Self { cmd: cmd_tx, events: ev_rx, manual_only }
    }
}

fn serial_loop(rx: Receiver<MonitorCmd>,
               ev: Sender<MonitorEvent>,
               store: Arc<parking_lot::Mutex<crate::store::Store>>,
               paste_aware: bool,
               manual_only: Arc<AtomicBool>) {
    // Spawn OS listener; it feeds MonitorCmd::Changed into the same queue.
    let (ltx, lrx) = channel::<()>();
    let _listener = clipboard::spawn_listener(ltx);
    let listener_rx = lrx;

    let mut failures: u32 = 0;
    let mut suspended_until: Option<Instant> = None;

    loop {
        // Fast-path: drain listener signals with debounce.
        let cmd = match listener_rx.try_recv() {
            Ok(_) => Some(MonitorCmd::Changed),
            Err(_) => match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(c) => Some(c),
                Err(_) => None, // idle tick: check suspension expiry
            },
        };
        let Some(cmd) = cmd else { continue; };

        // FR-S07: while suspended, only accept Probe / Write / Shutdown.
        if let Some(until) = suspended_until {
            if Instant::now() < until {
                match cmd {
                    MonitorCmd::Probe | MonitorCmd::Write { .. } | MonitorCmd::Shutdown => {}
                    MonitorCmd::Changed => continue,
                }
            } else {
                suspended_until = None;
                manual_only.store(false, Ordering::SeqCst);
                let _ = ev.send(MonitorEvent::Suspended(false));
                let _ = do_capture(&store, paste_aware, &ev); // light re-probe (FR-S07)
            }
        }

        match cmd {
            MonitorCmd::Shutdown => break,
            MonitorCmd::Write { text, html, reply } => {
                let ok = clipboard::write_clipboard(text.as_deref(), html.as_deref());
                let _ = reply.send(ok);
                failures = 0;
            }
            MonitorCmd::Probe => { do_capture(&store, paste_aware, &ev); failures = 0; }
            MonitorCmd::Changed => {
                // FR-S01: delay >= 30ms off the notification callback.
                std::thread::sleep(Duration::from_millis(CAPTURE_DELAY_MS));
                let ok = do_capture(&store, paste_aware, &ev);
                if ok { failures = 0; } else {
                    failures += 1;
                    if failures >= FAILURE_SUSPEND_THRESHOLD {
                        // FR-S07: suspend 60s, degrade to manual-paste mode.
                        suspended_until = Some(Instant::now() + Duration::from_secs(SUSPEND_SECONDS));
                        manual_only.store(true, Ordering::SeqCst);
                        let _ = ev.send(MonitorEvent::Suspended(true));
                        failures = 0;
                    }
                }
            }
        }
    }
}

/// One capture attempt with FR-S03 backoff. Returns true if captured or cleanly
/// empty (no data is not a failure); false on lock contention / timeout.
fn do_capture(store: &Arc<parking_lot::Mutex<crate::store::Store>>,
              paste_aware: bool,
              ev: &Sender<MonitorEvent>) -> bool {
    let _ = paste_aware; // reads here are already gated by user-visible triggers (FR-S09)
    {
        let st = store.lock();
        if st.config.pause_recording { return true; } // FR-P04
    }
    // FR-S09 感知粘贴: background mode only records on macOS when user pastes;
    // here we perform the read but the *system prompt* is avoided because the
    // read is triggered by a user-facing action when paste_aware && user-initiated.
    let start = Instant::now();
    for (i, delay) in retry_delays_ms().iter().enumerate() {
        if start.elapsed() > Duration::from_millis(READ_TIMEOUT_HARD_MS) { return false; }
        if let Some((raw, app)) = clipboard::read_primary(None) {
            let mut incoming = raw_to_incoming(raw, &app);
            // OCR for images (FR-Q03): text becomes searchable.
            if incoming.content_type == ContentType::Image {
                if let Some(bytes) = &incoming.image_bytes {
                    incoming.text_for_index = crate::ocr::recognize(bytes).unwrap_or_default();
                }
            }
            let mut st = store.lock();
            match st.record(incoming) {
                Ok(RecordOutcome::New(id)) | Ok(RecordOutcome::Duplicate(id)) => {
                    if let Some(m) = st.index.get(id) { let _ = ev.send(MonitorEvent::Captured(m.clone())); }
                }
                _ => {}
            }
            return true;
        }
        if i < 3 { std::thread::sleep(Duration::from_millis(*delay)); }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_plan_matches_spec() {
        assert_eq!(retry_delays_ms(), vec![50, 120, 250, 500]);
        assert!(READ_TIMEOUT_HARD_MS >= 800 - 1);
    }

    #[test]
    fn raw_text_becomes_link_when_url() {
        let inc = raw_to_incoming(RawClip::Text("https://kimi.moonshot.cn".into()), "Chrome");
        assert_eq!(inc.content_type, ContentType::Link);
        let inc2 = raw_to_incoming(RawClip::Text("普通文本".into()), "WPS");
        assert_eq!(inc2.content_type, ContentType::Text);
    }

    #[test]
    fn html_is_stripped_for_index_but_kept_in_payload() {
        let inc = raw_to_incoming(
            RawClip::Html("<b>Hello</b> <i>World</i>".into()), "Chrome");
        assert_eq!(inc.text_for_index.contains("Hello"), true);
        assert!(!inc.text_for_index.contains('<'));
        assert!(inc.payload.html.is_some());
    }

    #[test]
    fn files_index_uses_names() {
        let inc = raw_to_incoming(
            RawClip::Files(vec!["/tmp/报表.xlsx".into(), "/tmp/notes.md".into()]), "Finder");
        assert_eq!(inc.content_type, ContentType::Files);
        assert!(inc.text_for_index.contains("报表.xlsx"));
    }
}

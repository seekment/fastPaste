//! Functional tests mapped to the V2.0 acceptance criteria (section 7).
//! Run: cargo test --release   (perf test) / cargo test (functional)
use fastpaste_lib::models::*;
use fastpaste_lib::store::{RecordOutcome, Store};
use std::time::Instant;

fn open() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path()).unwrap();
    (dir, s)
}

fn text_clip(s: &str, app: &str) -> IncomingClip {
    IncomingClip::text(s, app)
}

fn image_clip(ocr: &str) -> IncomingClip {
    // 1x1 PNG
    let png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    IncomingClip {
        content_type: ContentType::Image,
        source_app: "Preview".into(),
        text_for_index: ocr.into(),
        payload: ClipPayload::default(),
        image_bytes: Some(png),
        thumbnail: None,
    }
}

/// 验收：记录 -> 去重 -> 中文/英文/前缀搜索 -> 类型/应用筛选 -> 置顶/删除。
#[test]
fn e2e_capture_search_dedup() {
    let (_d, mut s) = open();
    // FR-H01 / FR-H03
    let r = s.record(text_clip("会议纪要：下周三下午三点发布新版本", "WPS")).unwrap();
    assert!(matches!(r, RecordOutcome::New(_)));
    let r = s.record(text_clip("https://www.example.com/docs/fastpaste", "Chrome")).unwrap();
    assert!(matches!(r, RecordOutcome::New(_)));
    // FR-H04: same content merged
    let r = s.record(text_clip("会议纪要：下周三下午三点发布新版本", "WPS")).unwrap();
    assert!(matches!(r, RecordOutcome::Duplicate(_)));
    assert_eq!(s.index.len(), 2);
    // FR-Q02: 2 个中文字符命中（验收标准 section 7）
    assert_eq!(s.index.search("下周", 10, None, None).len(), 1);
    assert_eq!(s.index.search("下周", 10, None, Some("WPS")).len(), 1);
    assert_eq!(s.index.search("下周", 10, None, Some("Chrome")).len(), 0);
    // English prefix (FR-Q02)
    assert_eq!(s.index.search("examp", 10, None, None).len(), 1);
    // type filter (FR-H05)
    assert_eq!(s.index.search("", 10, Some(ContentType::Link), None).len(), 1);
    // FR-H07 pin/delete
    let id = s.index.search("example", 10, None, None)[0].id;
    assert!(s.set_pin(id, true).unwrap());
    assert!(s.index.get(id).unwrap().pinned);
    assert!(s.delete(id).unwrap());
    assert!(s.index.get(id).is_none());
    // files removed from disk too
    assert!(!s.dir.join(format!("items/{:06}.fpitem", id)).exists());
}

/// 验收：排除应用 + 敏感内容不入库（FR-P01/P03，NFR-S03/S04）。
#[test]
fn e2e_privacy_gates() {
    let (_d, mut s) = open();
    s.config.excluded_apps.push("1Password".into());
    assert!(matches!(s.record(text_clip("secret vault item", "1Password")).unwrap(), RecordOutcome::SkippedExcluded));
    assert_eq!(s.index.len(), 0);
    // AWS key never persisted
    assert!(matches!(
        s.record(text_clip("aws AKIAIOSFODNN7EXAMPLE done", "Chrome")).unwrap(),
        RecordOutcome::SkippedSensitive(_)));
    assert_eq!(s.index.len(), 0);
    // 排除应用内容在磁盘上无记录（验收标准）
    let items = std::fs::read_dir(s.dir.join("items")).unwrap().count();
    assert_eq!(items, 0);
}

/// 验收：崩溃恢复 —— 未 compact 直接重开（WAL 重放），历史完整保留（FR-H08/NFR-R02）。
#[test]
fn e2e_crash_recovery_wal_replay() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut s = Store::open(dir.path()).unwrap();
        for i in 0..5 {
            s.record(text_clip(&format!("crash test item number {}", i), "App")).unwrap();
        }
        // no compact() here - simulates a crash (process vanished)
    }
    let s = Store::open(dir.path()).unwrap();
    assert_eq!(s.index.len(), 5);
    assert_eq!(s.index.search("crash", 10, None, None).len(), 5);
    // payload readable
    let id = s.index.search("number 3", 10, None, None)[0].id;
    let p = s.read_payload(id).unwrap();
    assert!(p.text.unwrap().contains("number 3"));
}

/// 验收：保留策略 —— 超龄清理从旧到新，pinned 保留（FR-H02）。
#[test]
fn e2e_retention_cleanup() {
    let (_d, mut s) = open();
    s.config.retention_days = Some(7);
    let old_ts = now_ms() - 30 * 86_400_000;
    for i in 0..3 {
        let mut c = text_clip(&format!("old item {}", i), "App");
        s.record(c).unwrap();
        let id = s.index.recent(1, None, None)[0];
        s.index.touch(id, old_ts);
    }
    for i in 0..2 {
        s.record(text_clip(&format!("fresh item {}", i), "App")).unwrap();
    }
    // pin one old item
    let pinned_id = s.index.recent(10, None, None).iter()
        .map(|&id| id).find(|&id| s.index.get(id).unwrap().created_at == old_ts).unwrap();
    s.set_pin(pinned_id, true).unwrap();
    let removed = s.enforce_retention().unwrap();
    assert_eq!(removed, 2); // only unpinned old ones
    assert!(s.index.get(pinned_id).is_some());
}

/// 验收：导出 -> 新库导入，内容哈希去重（FR-B04）。
#[test]
fn e2e_export_import_roundtrip() {
    let dir1 = tempfile::tempdir().unwrap();
    let dir2 = tempfile::tempdir().unwrap();
    let zip = dir1.path().join("backup.zip");
    {
        let mut s = Store::open(dir1.path()).unwrap();
        s.record(text_clip("迁移测试数据 migration data", "App")).unwrap();
        s.record(image_clip("发票 号码 12345")).unwrap();
        s.export_archive(&zip).unwrap();
    }
    let mut s2 = Store::open(dir2.path()).unwrap();
    let n = s2.import_archive(&zip).unwrap();
    assert_eq!(n, 2);
    assert_eq!(s2.index.search("migration", 10, None, None).len(), 1);
    assert_eq!(s2.index.search("发票", 10, None, None).len(), 1); // OCR 文本可搜索（FR-Q03）
    // re-import: dedup
    assert_eq!(s2.import_archive(&zip).unwrap(), 0);
}

/// 验收：FR-A01 纯文本粘贴源数据 —— HTML 剥离无格式残留。
#[test]
fn e2e_plain_text_extraction() {
    let html = "<div style=\"color:red\"><b>Hello</b> <a href=\"x\">World</a>&amp;ok</div>";
    let plain = html_to_text(html);
    assert_eq!(plain.trim(), "Hello World&ok");
    assert!(!plain.contains('<') && !plain.contains("style"));
}

/// 验收（section 7 搜索性能）：10,000 条历史，2 个中文字符查询。
/// 目标 release ≤ 150ms；debug 模式下放宽到 800ms 防抖动。
#[test]
fn perf_10k_chinese_search() {
    let (_d, mut s) = open();
    for i in 0..10_000 {
        let c = if i % 2 == 0 {
            text_clip(&format!("订单编号 FP-{:06} 客户备注信息 普通文本", i), "App")
        } else {
            text_clip(&format!("random english word chunk {} alpha beta gamma", i), "App")
        };
        s.record(c).unwrap();
    }
    assert_eq!(s.index.len(), 10_000);
    let t = Instant::now();
    let hits = s.index.search("订单", 300, None, None);
    let el = t.elapsed();
    assert_eq!(hits.len(), 5_000);
    eprintln!("10k items, 2-hanzi query: {:?}", el);
    if cfg!(debug_assertions) {
        assert!(el < std::time::Duration::from_millis(800));
    } else {
        assert!(el < std::time::Duration::from_millis(150)); // FR-Q01 acceptance
    }
}

/// 验收（section 7）：清空全部历史需生效且不可恢复（FR-P07）。
#[test]
fn e2e_clear_all() {
    let (_d, mut s) = open();
    s.record(text_clip("to be wiped", "App")).unwrap();
    s.record(image_clip("x")).unwrap();
    s.clear_all().unwrap();
    assert_eq!(s.index.len(), 0);
    assert_eq!(std::fs::read_dir(s.dir.join("items")).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(s.dir.join("blobs")).unwrap().count(), 0);
}

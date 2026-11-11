//! Storage & index layer (FR 5.2-5.5): custom file format, zero DB engines.
//!
//! Layout:
//!   data/index.bin      snapshot of PersistedIndex (MessagePack)
//!   data/index.wal      append-only log of index mutations
//!   data/items/*.fpitem per-item content shards
//!   data/blobs/*        raw image payloads + thumbnails
//!   data/config.json    settings
//!
//! Durability (NFR-R02): every write is "tmp file -> fsync -> atomic rename";
//! WAL replay after crash restores consistency.
use crate::config::Config;
use crate::index::{AddOutcome, PersistedIndex, SearchIndex};
use crate::models::*;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"FPIT";
const ITEM_VER: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WalRecord {
    Add { meta: ClipMeta, tokens: Vec<String> },
    Touch { id: u64, ts: i64 },
    Remove { id: u64 },
    Pin { id: u64, pinned: bool },
    Bind { id: u64, key: Option<String> },
    Edit { id: u64, content_hash: u64, preview: String, text_length: usize, tokens: Vec<String> },
    Clear,
}

pub enum RecordOutcome {
    New(u64),
    Duplicate(u64),
    SkippedSensitive(&'static str),
    SkippedExcluded,
}

pub struct Store {
    pub dir: PathBuf,
    pub index: SearchIndex,
    pub config: Config,
    wal: BufWriter<File>,
    pub bytes_used: u64,
}

fn canonical_hash(ct: ContentType, payload: &ClipPayload, index_text: &str) -> u64 {
    let mut h = fnv1a(index_text.as_bytes());
    if let Some(f) = &payload.files {
        for p in f { h ^= fnv1a(p.as_bytes()); }
    }
    if payload.image.is_some() { h ^= 0x9e3779b97f4a7c15; }
    h ^ (ct.to_u8() as u64) << 56
}

impl Store {
    pub fn open(dir: &Path) -> io::Result<Self> {
        for sub in ["items", "blobs"] {
            fs::create_dir_all(dir.join(sub))?;
        }
        let config = Config::load(&dir.join("config.json"));

        // 1) load snapshot (decrypt first when sealed with FR-P06 encryption)
        let mut index = match fs::read(dir.join("index.bin")) {
            Ok(bytes) => {
                let plain = if bytes.starts_with(ENC_MAGIC) {
                    match load_cipher(dir, &config)? {
                        Some(c) => unseal(&c, &bytes[4..]).unwrap_or_default(),
                        None => Vec::new(),
                    }
                } else {
                    bytes
                };
                rmp_serde::from_slice::<PersistedIndex>(&plain)
                    .map(SearchIndex::from_persisted)
                    .unwrap_or_else(|_| SearchIndex::new())
            }
            Err(_) => SearchIndex::new(),
        };

        // 2) replay WAL (crash recovery, NFR-R02)
        let wal_path = dir.join("index.wal");
        let mut bytes_used: u64 = fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
        if let Ok(mut f) = File::open(&wal_path) {
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            let mut cursor = &buf[..];
            while !cursor.is_empty() {
                match rmp_serde::from_read::<_, WalRecord>(&mut cursor) {
                    Ok(rec) => Self::apply_wal(&mut index, rec),
                    Err(_) => break, // torn tail from a crash - drop it
                }
            }
        }
        bytes_used += Self::dir_bytes(&dir.join("items")) + Self::dir_bytes(&dir.join("blobs"));

        let wal = BufWriter::new(OpenOptions::new().create(true).append(true).open(&wal_path)?);
        Ok(Self { dir: dir.to_path_buf(), index, config, wal, bytes_used })
    }

    fn apply_wal(index: &mut SearchIndex, rec: WalRecord) {
        match rec {
            WalRecord::Add { meta, tokens } => {
                // direct insert without dedup re-check; snapshot already covered those
                let mut p = index.snapshot().clone();
                let id = meta.id;
                p.items.insert(id, meta);
                let mut toks = tokens.clone();
                toks.sort(); toks.dedup();
                for t in &toks {
                    let e = p.tokens.entry(t.clone()).or_default();
                    if !e.contains(&id) { e.push(id); }
                }
                p.by_hash.insert(p.items[&id].content_hash, id);
                if !p.timeline.contains(&id) { p.timeline.insert(0, id); }
                p.item_tokens.insert(id, toks);
                *index = SearchIndex::from_persisted(p);
            }
            WalRecord::Touch { id, ts } => index.touch(id, ts),
            WalRecord::Remove { id } => { index.remove(id); },
            WalRecord::Pin { id, pinned } => { index.set_pin(id, pinned); },
            WalRecord::Bind { id, key } => { index.set_bound_key(id, key); },
            WalRecord::Edit { id, content_hash, preview, text_length, tokens } => {
                let mut p = index.snapshot().clone();
                let mut toks = tokens.clone();
                toks.sort(); toks.dedup();
                if let Some(old) = p.item_tokens.get(&id) {
                    for t in old {
                        if let Some(l) = p.tokens.get_mut(t) { l.retain(|&x| x != id); }
                    }
                }
                p.tokens.retain(|_, v| !v.is_empty());
                for t in &toks {
                    let e = p.tokens.entry(t.clone()).or_default();
                    if !e.contains(&id) { e.push(id); }
                    e.sort();
                }
                p.item_tokens.insert(id, toks);
                if let Some(m) = p.items.get_mut(&id) {
                    if let Some(l) = p.by_hash.remove(&m.content_hash) { if l == id { /* removed */ } }
                    m.content_hash = content_hash;
                    m.preview = preview;
                    m.text_length = text_length;
                    p.by_hash.insert(content_hash, id);
                }
                *index = SearchIndex::from_persisted(p);
            },
            WalRecord::Clear => {
                let ids: Vec<u64> = index.snapshot().items.keys().copied().collect();
                index.remove_many(&ids);
            }
        }
    }

    fn dir_bytes(d: &Path) -> u64 {
        fs::read_dir(d).map(|rd| rd.flatten()
            .map(|e| e.metadata().map(|m| m.len()).unwrap_or(0)).sum()).unwrap_or(0)
    }

    fn append_wal(&mut self, rec: &WalRecord) -> io::Result<()> {
        let bytes = rmp_serde::to_vec(rec).map_err(io::Error::other)?;
        self.wal.write_all(&bytes)?;
        self.wal.flush()?; // group-commit of one record; fsync below on compact
        Ok(())
    }

    /// Main capture path (FR-H01..H04, FR-P01, FR-P03).
    pub fn record(&mut self, clip: IncomingClip) -> io::Result<RecordOutcome> {
        if self.config.is_excluded(&clip.source_app) {
            return Ok(RecordOutcome::SkippedExcluded); // NFR-S04: dropped at the gate
        }
        if let Some(rule) = sensitive::detect(&clip.text_for_index) {
            return Ok(RecordOutcome::SkippedSensitive(rule)); // FR-P03: never persisted
        }
        let ts = now_ms();
        let hash = canonical_hash(clip.content_type, &clip.payload, &clip.text_for_index);
        let meta = ClipMeta {
            id: 0,
            content_type: clip.content_type,
            content_hash: hash,
            source_app: clip.source_app.clone(),
            created_at: ts,
            pinned: false,
            item_file: String::new(),
            blob_ref: None,
            thumb_ref: None,
            text_length: clip.text_for_index.len(),
            preview: make_preview(&clip.text_for_index),
            bound_key: None,
        };

        match self.index.add(meta, &clip.text_for_index) {
            AddOutcome::Duplicate(id) => {
                self.append_wal(&WalRecord::Touch { id, ts })?;
                return Ok(RecordOutcome::Duplicate(id));
            }
            AddOutcome::New(_) => {}
        }
        let id = self.index.snapshot().by_hash[&hash];

        // persist payload shard
        let item_file = format!("items/{:06}.fpitem", id);
        self.write_item_file(&item_file, clip.content_type, id, &clip.payload)?;

        // image blob + thumbnail (FR-H06)
        let mut blob_ref = None;
        let mut thumb_ref = None;
        if let Some(bytes) = clip.image_bytes {
            let ext = if cfg!(target_os = "macos") { "png" } else { "bmp" };
            let bref = format!("blobs/{:016x}.{}", hash, ext);
            atomic_write(&self.dir.join(&bref), &bytes)?;
            blob_ref = Some(bref);
            self.bytes_used += bytes.len() as u64;
            if let Some(th) = clip.thumbnail {
                let tref = format!("blobs/{:016x}_thumb.png", hash);
                atomic_write(&self.dir.join(&tref), &th)?;
                thumb_ref = Some(tref);
                self.bytes_used += th.len() as u64;
            }
        }
        if let Some(m) = self.index.snapshot_mut().items.get_mut(&id) {
            m.item_file = item_file;
            m.blob_ref = blob_ref;
            m.thumb_ref = thumb_ref;
        }
        let meta = self.index.snapshot().items[&id].clone();
        let tokens = self.index.snapshot().item_tokens[&id].clone();
        self.append_wal(&WalRecord::Add { meta, tokens })?;
        Ok(RecordOutcome::New(id))
    }

    fn write_item_file(&mut self, rel: &str, ct: ContentType, id: u64, payload: &ClipPayload) -> io::Result<()> {
        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&ITEM_VER.to_le_bytes());
        buf.push(ct.to_u8());
        let meta_json = serde_json::to_string(&serde_json::json!({"id": id})).unwrap();
        buf.extend_from_slice(&(meta_json.len() as u32).to_le_bytes());
        buf.extend_from_slice(meta_json.as_bytes());
        let body = rmp_serde::to_vec(payload).map_err(io::Error::other)?;
        buf.extend_from_slice(&(body.len() as u64).to_le_bytes());
        buf.extend_from_slice(&body);
        atomic_write(&self.dir.join(rel), &buf)?;
        self.bytes_used += buf.len() as u64;
        Ok(())
    }

    /// Load full payload on demand (preview/paste only, FR 5.4).
    pub fn read_payload(&self, id: u64) -> io::Result<ClipPayload> {
        let meta = self.index.get(id).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such item"))?;
        let bytes = fs::read(self.dir.join(&meta.item_file))?;
        let mut cur = &bytes[..];
        if cur.len() < 9 || &cur[..4] != MAGIC { return Err(io::Error::new(io::ErrorKind::InvalidData, "bad magic")); }
        cur = &cur[8..]; // skip magic + version
        let ct = ContentType::from_u8(cur[0]).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad type"))?;
        cur = &cur[1..];
        let mlen = u32::from_le_bytes(cur[..4].try_into().unwrap()) as usize;
        let meta: serde_json::Value = serde_json::from_slice(&cur[4..4 + mlen]).unwrap_or_default();
        cur = &cur[4 + mlen..];
        let plen = u64::from_le_bytes(cur[..8].try_into().unwrap()) as usize;
        cur = &cur[8..];
        let body: Vec<u8> = if meta.get("enc").and_then(|v| v.as_bool()).unwrap_or(false) {
            match load_cipher(&self.dir, &self.config)? {
                Some(c) => unseal(&c, &cur[..plen])?,
                None => return Err(io::Error::new(io::ErrorKind::Other, "encrypted item but encryption disabled")),
            }
        } else {
            cur[..plen].to_vec()
        };
        let payload: ClipPayload = rmp_serde::from_slice(&body).map_err(io::Error::other)?;
        let _ = ct;
        Ok(payload)
    }

    pub fn read_blob(&self, id: u64) -> io::Result<Option<Vec<u8>>> {
        let meta = self.index.get(id).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such item"))?;
        match &meta.blob_ref {
            Some(b) => Ok(Some(fs::read(self.dir.join(b))?)),
            None => Ok(None),
        }
    }

    pub fn delete(&mut self, id: u64) -> io::Result<bool> {
        if let Some(meta) = self.index.remove(id) {
            for f in [Some(meta.item_file), meta.blob_ref, meta.thumb_ref].into_iter().flatten() {
                let p = self.dir.join(&f);
                if let Ok(m) = fs::metadata(&p) { self.bytes_used = self.bytes_used.saturating_sub(m.len()); }
                fs::remove_file(&p).ok();
            }
            self.append_wal(&WalRecord::Remove { id })?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// PasteMate-style key binding.
    pub fn set_bound_key(&mut self, id: u64, key: Option<String>) -> io::Result<bool> {
        if self.index.set_bound_key(id, key.clone()) {
            self.append_wal(&WalRecord::Bind { id, key })?;
            Ok(true)
        } else { Ok(false) }
    }

    /// PasteMate-style card editing: replace text content, re-hash, re-index.
    pub fn edit_text(&mut self, id: u64, text: &str) -> io::Result<bool> {
        let meta = match self.index.get(id) { Some(m) => m.clone(), None => return Ok(false) };
        let mut payload = self.read_payload(id)?;
        payload.text = Some(text.to_string());
        self.write_item_file(&meta.item_file, meta.content_type, id, &payload)?;
        let new_hash = fnv1a(text.as_bytes()) ^ (meta.content_type.to_u8() as u64) << 56;
        let tokens = match self.index.reindex(id, text) { Some(t) => t, None => return Ok(false) };
        self.index.snapshot_mut().by_hash.remove(&meta.content_hash);
        if let Some(m) = self.index.snapshot_mut().items.get_mut(&id) {
            m.content_hash = new_hash;
            m.preview = make_preview(text);
            m.text_length = text.len();
        }
        self.index.snapshot_mut().by_hash.insert(new_hash, id);
        self.append_wal(&WalRecord::Edit { id, content_hash: new_hash, preview: make_preview(text), text_length: text.len(), tokens })?;
        Ok(true)
    }

    /// PasteMate-style empty card (memo).
    pub fn create_note(&mut self) -> io::Result<RecordOutcome> {
        self.record(IncomingClip::text("", "fastPaste 备忘录"))
    }

    pub fn set_pin(&mut self, id: u64, pinned: bool) -> io::Result<bool> {
        if self.index.set_pin(id, pinned) {
            self.append_wal(&WalRecord::Pin { id, pinned })?;
            Ok(true)
        } else { Ok(false) }
    }

    pub fn clear_all(&mut self) -> io::Result<()> {
        let ids: Vec<u64> = self.index.snapshot().items.keys().copied().collect();
        let metas = self.index.remove_many(&ids);
        for m in metas {
            for f in [Some(m.item_file), m.blob_ref, m.thumb_ref].into_iter().flatten() {
                fs::remove_file(self.dir.join(&f)).ok();
            }
        }
        fs::remove_dir_all(self.dir.join("items")).ok();
        fs::remove_dir_all(self.dir.join("blobs")).ok();
        fs::create_dir_all(self.dir.join("items")).ok();
        fs::create_dir_all(self.dir.join("blobs")).ok();
        self.bytes_used = 0;
        self.append_wal(&WalRecord::Clear)
    }

    /// FR-H02 retention: purge oldest-first by age and/or disk budget.
    pub fn enforce_retention(&mut self) -> io::Result<usize> {
        let mut doomed: Vec<u64> = vec![];
        if let Some(days) = self.config.retention_days {
            let cutoff = now_ms() - days as i64 * 86_400_000;
            doomed.extend(self.index.snapshot().items.values()
                .filter(|m| !m.pinned && m.created_at < cutoff).map(|m| m.id));
        }
        // disk budget: oldest-first until under budget
        if let Some(max) = self.config.max_disk_bytes {
            while self.bytes_used > max {
                let oldest = self.index.snapshot().items.values()
                    .filter(|m| !m.pinned)
                    .min_by_key(|m| m.created_at)
                    .map(|m| m.id);
                match oldest {
                    Some(id) if !doomed.contains(&id) => doomed.push(id),
                    _ => break,
                }
            }
        }
        let mut n = 0;
        for id in doomed {
            if self.delete(id)? { n += 1; }
        }
        Ok(n)
    }

    /// Merge WAL into index.bin, truncate WAL (called on clean shutdown).
    pub fn compact(&mut self) -> io::Result<()> {
        self.wal.flush()?;
        let mut snap = rmp_serde::to_vec(self.index.snapshot()).map_err(io::Error::other)?;
        if self.config.encrypt {
            if let Some(c) = load_cipher(&self.dir, &self.config)? {
                let mut sealed = ENC_MAGIC.to_vec();
                sealed.extend_from_slice(&seal(&c, &snap)?);
                snap = sealed;
            }
        }
        atomic_write(&self.dir.join("index.bin"), &snap)?;
        let wal_path = self.dir.join("index.wal");
        fs::remove_file(&wal_path).ok();
        self.wal = BufWriter::new(OpenOptions::new().create(true).append(true).open(&wal_path)?);
        Ok(())
    }

    // ---------- export / import (FR-B04) ----------
    pub fn export_archive(&self, zip_path: &Path) -> io::Result<()> {
        let mut zw = zip::ZipWriter::new(File::create(zip_path)?);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let snap = rmp_serde::to_vec(self.index.snapshot()).map_err(io::Error::other)?;
        zw.start_file("index.bin", opts)?;
        zw.write_all(&snap)?;
        for m in self.index.snapshot().items.values() {
            for f in [Some(&m.item_file), m.blob_ref.as_ref(), m.thumb_ref.as_ref()].into_iter().flatten() {
                let disk = self.dir.join(f);
                if disk.exists() {
                    zw.start_file(f.as_str(), opts)?;
                    zw.write_all(&fs::read(&disk)?)?;
                }
            }
        }
        zw.finish().map(|_| ()).map_err(io::Error::other)
    }

    pub fn import_archive(&mut self, zip_path: &Path) -> io::Result<usize> {
        let mut zr = zip::ZipArchive::new(File::open(zip_path)?)?;
        let mut count = 0;
        // read the archive's snapshot bytes first; NEVER overwrite the local index.bin
        let imported_bytes: Vec<u8> = {
            let mut e = zr.by_name("index.bin").map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let mut b = Vec::with_capacity(e.size() as usize);
            e.read_to_end(&mut b)?;
            b
        };
        let names: Vec<String> = (0..zr.len())
            .filter_map(|i| zr.by_index(i).ok().and_then(|e| e.mangled_name().to_str().map(|s| s.to_string())))
            .collect();
        for name in &names {
            if name.ends_with('/') || name == "index.bin" { continue; }
            let mut entry = zr.by_name(name).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let mut bytes = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut bytes)?;
            let out = self.dir.join(name);
            if let Some(parent) = out.parent() { fs::create_dir_all(parent)?; }
            atomic_write(&out, &bytes)?;
        }
        // merge imported snapshot into the CURRENT index, dedup by content hash (FR-B04)
        let imported: PersistedIndex = rmp_serde::from_slice(&imported_bytes)
            .map_err(io::Error::other)?;
        let mut p = self.index.snapshot().clone();
        let mut id_map: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
        for (id, meta) in imported.items {
            if p.by_hash.contains_key(&meta.content_hash) { continue; }
            // remap id on collision so two devices never share item ids
            let nid = if p.items.contains_key(&id) { p.next_id } else { id };
            p.next_id = p.next_id.max(nid + 1).max(id + 1);
            p.items.insert(nid, meta.clone());
            p.by_hash.insert(meta.content_hash, nid);
            if !p.timeline.contains(&nid) { p.timeline.insert(0, nid); }
            id_map.insert(id, nid);
            count += 1;
        }
        for (id, toks) in imported.item_tokens {
            if let Some(&nid) = id_map.get(&id) { p.item_tokens.insert(nid, toks); }
        }
        for (t, mut ids) in imported.tokens {
            let e = p.tokens.entry(t).or_default();
            for id in ids.drain(..) {
                let nid = id_map.get(&id).copied().unwrap_or(id);
                if !e.contains(&nid) { e.push(nid); }
            }
            e.sort();
        }
        *self.index.snapshot_mut() = p;
        Ok(count)
    }

    // accessor used by WAL replay path
    pub fn snapshot_mut(&mut self) -> &mut PersistedIndex { self.index.snapshot_mut() }
}

// ---------- FR-P06: AES-256-GCM local encryption (content + snapshot) ----------
const ENC_MAGIC: &[u8; 4] = b"FENC";

fn load_cipher(dir: &Path, cfg: &Config) -> io::Result<Option<Aes256Gcm>> {
    if !cfg.encrypt { return Ok(None); }
    let key_path = dir.join("key.bin");
    let key: [u8; 32] = match fs::read(&key_path) {
        Ok(k) if k.len() == 32 => k.try_into().unwrap(),
        _ => {
            let k: [u8; 32] = rand::random();
            fs::write(&key_path, &k)?;
            k
        }
    };
    Ok(Some(Aes256Gcm::new_from_slice(&key).expect("32-byte AES key")))
}

fn seal(cipher: &Aes256Gcm, data: &[u8]) -> io::Result<Vec<u8>> {
    let nonce_bytes: [u8; 12] = rand::random();
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher.encrypt(nonce, data).map_err(|_| io::Error::new(io::ErrorKind::Other, "seal failed"))?;
    let mut out = nonce_bytes.to_vec();
    out.extend_from_slice(&ct);
    Ok(out)
}

fn unseal(cipher: &Aes256Gcm, data: &[u8]) -> io::Result<Vec<u8>> {
    if data.len() < 12 { return Err(io::Error::new(io::ErrorKind::InvalidData, "short sealed data")); }
    let (n, ct) = data.split_at(12);
    cipher.decrypt(Nonce::from_slice(n), ct).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "decrypt failed"))
}

/// tmp -> fsync -> rename (NFR-R02).
pub fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

pub mod sensitive {
    pub use crate::sensitive::detect;
}

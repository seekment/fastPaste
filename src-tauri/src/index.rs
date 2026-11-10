//! In-memory search index with persistence snapshot + WAL (FR-Q01/Q02/Q06/Q07).
//! Index stores only token -> item-id mappings and lightweight metadata;
//! full content stays in .fpitem files (FR 5.3, 5.4).
use crate::models::{ClipMeta, ContentType};
use crate::tokenizer::tokenize;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Compact snapshot persisted to index.bin.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersistedIndex {
    pub items: HashMap<u64, ClipMeta>,
    pub item_tokens: HashMap<u64, Vec<String>>,
    pub tokens: BTreeMap<String, Vec<u64>>, // token -> item ids (sorted, dedup)
    pub by_hash: HashMap<u64, u64>,         // content_hash -> item id
    pub timeline: Vec<u64>,                 // item ids, newest first
    pub next_id: u64,
}

pub struct SearchIndex {
    p: PersistedIndex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddOutcome {
    New(u64),
    Duplicate(u64), // existing id, timestamp refreshed (FR-H04)
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub id: u64,
    pub score: i32,
}

impl SearchIndex {
    pub fn new() -> Self {
        Self { p: PersistedIndex { next_id: 1, ..Default::default() } }
    }

    pub fn from_persisted(p: PersistedIndex) -> Self {
        let mut p = p;
        if p.next_id == 0 {
            p.next_id = p.items.keys().max().copied().unwrap_or(0) + 1;
        }
        Self { p }
    }

    pub fn snapshot(&self) -> &PersistedIndex { &self.p }

    pub fn get(&self, id: u64) -> Option<&ClipMeta> { self.p.items.get(&id) }

    pub fn len(&self) -> usize { self.p.items.len() }

    pub fn add(&mut self, mut meta: ClipMeta, text_for_index: &str) -> AddOutcome {
        if let Some(&existing) = self.p.by_hash.get(&meta.content_hash) {
            // FR-H04: merge duplicate, only refresh timestamp & move to front
            if let Some(m) = self.p.items.get_mut(&existing) {
                m.created_at = meta.created_at;
            }
            self.move_to_front(existing);
            return AddOutcome::Duplicate(existing);
        }
        let id = self.p.next_id;
        self.p.next_id += 1;
        meta.id = id;
        let mut toks: Vec<String> = tokenize(text_for_index);
        toks.sort();
        toks.dedup();
        for t in &toks {
            let e = self.p.tokens.entry(t.clone()).or_default();
            if let Err(pos) = e.binary_search(&id) { e.insert(pos, id); }
        }
        self.p.timeline.insert(0, id);
        self.p.by_hash.insert(meta.content_hash, id);
        self.p.item_tokens.insert(id, toks);
        self.p.items.insert(id, meta);
        AddOutcome::New(id)
    }

    fn move_to_front(&mut self, id: u64) {
        self.p.timeline.retain(|&x| x != id);
        self.p.timeline.insert(0, id);
    }

    pub fn touch(&mut self, id: u64, ts: i64) {
        if let Some(m) = self.p.items.get_mut(&id) { m.created_at = ts; }
        self.move_to_front(id);
    }

    pub fn set_pin(&mut self, id: u64, pinned: bool) -> bool {
        match self.p.items.get_mut(&id) {
            Some(m) => { m.pinned = pinned; true }
            None => false,
        }
    }

    pub fn remove(&mut self, id: u64) -> Option<ClipMeta> {
        let meta = self.p.items.remove(&id)?;
        self.p.by_hash.remove(&meta.content_hash);
        self.p.timeline.retain(|&x| x != id);
        if let Some(toks) = self.p.item_tokens.remove(&id) {
            for t in toks {
                if let Some(list) = self.p.tokens.get_mut(&t) {
                    list.retain(|&x| x != id);
                }
            }
        }
        self.p.tokens.retain(|_, v| !v.is_empty());
        Some(meta)
    }

    pub fn remove_many(&mut self, ids: &[u64]) -> Vec<ClipMeta> {
        ids.iter().filter_map(|&id| self.remove(id)).collect()
    }

    /// Default listing: pinned first (by pin time order approx: pinned first then timeline).
    pub fn recent(&self, limit: usize, ctype: Option<ContentType>, app: Option<&str>) -> Vec<u64> {
        let ids: Vec<u64> = self.p.items.values()
            .filter(|m| ctype.map_or(true, |c| m.content_type == c))
            .filter(|m| app.map_or(true, |a| m.source_app == a))
            .map(|m| m.id)
            .collect();
        let mut v: Vec<(bool, i64, u64)> = ids.into_iter()
            .map(|id| { let m = &self.p.items[&id]; (m.pinned, m.created_at, id) })
            .collect();
        v.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        v.into_iter().take(limit).map(|x| x.2).collect()
    }

    /// AND-style search. Ranking (FR-Q04): exact(3) > prefix(2) > substring(1) per token,
    /// then newest first.
    pub fn search(&self, query: &str, limit: usize, ctype: Option<ContentType>, app: Option<&str>) -> Vec<Hit> {
        let qtoks = tokenize(query);
        let mut candidates: Option<BTreeSet<u64>> = None;
        for qt in &qtoks {
            let mut set: BTreeSet<u64> = BTreeSet::new();
            // exact + prefix scan over sorted token map
            for (tok, list) in self.p.tokens.range(qt.clone()..) {
                if !tok.starts_with(qt.as_str()) { break; }
                for &id in list { set.insert(id); }
            }
            candidates = Some(match candidates {
                None => set,
                Some(prev) => prev.intersection(&set).copied().collect(),
            });
        }

        let mut scored: Vec<Hit> = Vec::new();
        let ids: Vec<u64> = match (&candidates, qtoks.is_empty()) {
            (Some(set), _) => set.iter().copied().collect(),
            (None, true) => self.p.timeline.clone(), // empty query -> recency listing
            (None, false) => vec![],
        };
        for id in ids {
            let m = match self.p.items.get(&id) { Some(m) => m, None => continue };
            if ctype.map_or(false, |c| m.content_type != c) { continue; }
            if app.map_or(false, |a| m.source_app != a) { continue; }
            let mut score = 0i32;
            if !qtoks.is_empty() {
                let item_toks = match self.p.item_tokens.get(&id) { Some(t) => t, None => continue };
                let mut all_match = true;
                for qt in &qtoks {
                    let q = Self::token_quality(item_toks, qt);
                    if q == 0 { all_match = false; break; }
                    score += q;
                }
                if !all_match { continue; }
            }
            scored.push(Hit { id, score });
        }
        scored.sort_by(|a, b| {
            b.score.cmp(&a.score)
                .then(self.p.items[&b.id].created_at.cmp(&self.p.items[&a.id].created_at))
        });
        scored.truncate(limit);
        scored
    }

    fn token_quality(item_toks: &[String], qt: &str) -> i32 {
        let mut best = 0;
        for t in item_toks {
            if t == qt { best = 3; break; }
            if best < 2 && t.starts_with(qt) { best = 2; }
            if best < 1 && t.contains(qt) { best = 1; }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{fnv1a, now_ms};

    fn meta(text: &str, ts: i64) -> ClipMeta {
        ClipMeta {
            id: 0,
            content_type: ContentType::Text,
            content_hash: fnv1a(text.as_bytes()),
            source_app: "Test".into(),
            created_at: ts,
            pinned: false,
            item_file: String::new(),
            blob_ref: None,
            thumb_ref: None,
            text_length: text.len(),
            preview: text.into(),
        }
    }

    #[test]
    fn add_and_search_chinese_bigram() {
        let mut idx = SearchIndex::new();
        idx.add(meta("会议纪要：下周三发布", 1000), "会议纪要：下周三发布");
        let hits = idx.search("下周", 10, None, None);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].score, 9); // 下 + 周 + 下周, all exact (3+3+3)
    }

    #[test]
    fn english_prefix_match_and_ranking() {
        let mut idx = SearchIndex::new();
        idx.add(meta("world", 1000), "world");
        idx.add(meta("word processing", 2000), "word processing");
        let hits = idx.search("wor", 10, None, None);
        assert_eq!(hits.len(), 2);
        // exact "wor" doesn't exist; both prefix(2); newer first
        assert_eq!(hits[0].id, 2);
    }

    #[test]
    fn dedup_merges_and_refreshes() {
        let mut idx = SearchIndex::new();
        let r1 = idx.add(meta("same text", 1000), "same text");
        let id1 = match r1 { AddOutcome::New(id) => id, _ => panic!() };
        let r2 = idx.add(meta("same text", 2000), "same text");
        match r2 { AddOutcome::Duplicate(id) => assert_eq!(id, id1), _ => panic!() };
        assert_eq!(idx.len(), 1);
        assert_eq!(idx.get(id1).unwrap().created_at, 2000);
    }

    #[test]
    fn remove_purges_index() {
        let mut idx = SearchIndex::new();
        let r = idx.add(meta("temporary item", 1), "temporary item");
        let id = match r { AddOutcome::New(id) => id, _ => panic!() };
        assert_eq!(idx.search("temporary", 10, None, None).len(), 1);
        idx.remove(id);
        assert!(idx.search("temporary", 10, None, None).is_empty());
        assert!(idx.p.tokens.is_empty());
    }

    #[test]
    fn empty_query_returns_recency() {
        let mut idx = SearchIndex::new();
        idx.add(meta("a", 1), "a");
        idx.add(meta("b", 2), "b");
        let hits = idx.search("", 10, None, None);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, 2); // newest first
    }
}

impl SearchIndex {
    /// Mutable access to the persisted snapshot (used by store WAL replay/import).
    pub fn snapshot_mut(&mut self) -> &mut PersistedIndex { &mut self.p }
}

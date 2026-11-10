// Mirror of src/tokenizer.rs (FR-Q02): english lowercase words + CJK unigrams/bigrams.
(function (global) {
  function isHan(c) {
    const code = c.codePointAt(0);
    return (code >= 0x4e00 && code <= 0x9fff) || (code >= 0x3400 && code <= 0x4dbf);
  }
  function tokenize(text) {
    const out = [];
    let word = '';
    let hanRun = [];
    const flushHan = () => {
      if (!hanRun.length) return;
      for (const c of hanRun) out.push(c);
      for (let i = 0; i + 1 < hanRun.length; i++) out.push(hanRun[i] + hanRun[i + 1]);
      hanRun = [];
    };
    for (const c of String(text)) {
      if (isHan(c)) {
        if (word) { out.push(word); word = ''; }
        hanRun.push(c);
      } else if (/[A-Za-z0-9_]/.test(c)) {
        flushHan();
        word += c.toLowerCase();
      } else {
        if (word) { out.push(word); word = ''; }
        flushHan();
      }
    }
    if (word) out.push(word);
    flushHan();
    return out;
  }
  // score one query token against item tokens: exact(3) > prefix(2) > substring(1)
  function tokenQuality(itemTokens, qt) {
    let best = 0;
    for (const t of itemTokens) {
      if (t === qt) return 3;
      if (best < 2 && t.startsWith(qt)) best = 2;
      if (best < 1 && t.includes(qt)) best = 1;
    }
    return best;
  }
  function scoreQuery(query, itemTokens) {
    const qts = tokenize(query);
    if (!qts.length) return 0;
    let s = 0;
    for (const qt of qts) {
      const q = tokenQuality(itemTokens, qt);
      if (!q) return -1; // AND semantics
      s += q;
    }
    return s;
  }
  const api = { tokenize, tokenQuality, scoreQuery };
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  else global.FPTokenizer = api;
})(typeof window !== 'undefined' ? window : globalThis);

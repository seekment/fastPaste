// node frontend/tests/tokenizer.test.js
const { tokenize, scoreQuery } = require('../tokenizer.js');
const assert = require('assert');

// FR-Q02: bigram + unigram
const toks = new Set(tokenize('复制粘贴'));
assert(toks.has('复制') && toks.has('制粘') && toks.has('贴'), 'bigram/unigram');

// English lowercased
assert(new Set(tokenize('Hello World')).has('hello'));

// 单字命中
assert(tokenize('发票抬头').includes('票'));

// CJK queries expand to unigrams+bigrams (same as Rust tokenizer):
// '纪要' -> ['纪','要','纪要'], each exact-matches -> 3+3+3 = 9
const doc1 = tokenize('会议纪要 发布计划');
const doc2 = tokenize('计算器 技术方案');
assert.strictEqual(scoreQuery('纪要', doc1), 9);
assert.strictEqual(scoreQuery('计', doc1), 3);   // unigram exact
assert.strictEqual(scoreQuery('技术', doc2), 9);
assert.strictEqual(scoreQuery('不存在词', doc1), -1); // AND miss

// ranking: exact beats prefix (FR-Q04)
const docA = tokenize('world peace');
const docB = tokenize('wordsmith craft');
assert.strictEqual(scoreQuery('world', docA), 3); // exact
assert.strictEqual(scoreQuery('word', docB), 2);  // prefix only
assert.ok(scoreQuery('world', docA) > scoreQuery('word', docB));

console.log('tokenizer.test.js: all assertions passed');

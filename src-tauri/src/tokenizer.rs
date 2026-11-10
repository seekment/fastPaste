//! Query/document tokenizer (FR-Q02):
//! - English words -> lowercased full-word tokens, matched by prefix at query time.
//! - Chinese text  -> bigram (二元组) tokens; unigrams are also emitted so a
//!   single Han character can hit (FR-Q02: 1-2 个汉字即可命中).
//! No external segmentation dictionary is required.

fn is_han(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
        || ('\u{3400}'..='\u{4dbf}').contains(&c)
}

pub fn tokenize(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut word = String::new(); // current latin/digit word
    let mut han_run: Vec<char> = Vec::new();

    let mut flush_han = |run: &mut Vec<char>, out: &mut Vec<String>| {
        if run.is_empty() { return; }
        // unigrams
        for c in run.iter() { out.push(c.to_string()); }
        // bigrams
        for w in run.windows(2) {
            let mut s = String::new();
            s.push(w[0]); s.push(w[1]);
            out.push(s);
        }
        run.clear();
    };

    for c in text.chars() {
        if is_han(c) {
            if !word.is_empty() { out.push(std::mem::take(&mut word)); }
            han_run.push(c);
        } else if c.is_alphanumeric() || c == '_' {
            flush_han(&mut han_run, &mut out);
            word.extend(c.to_lowercase());
        } else {
            if !word.is_empty() { out.push(std::mem::take(&mut word)); }
            flush_han(&mut han_run, &mut out);
        }
    }
    if !word.is_empty() { out.push(word); }
    flush_han(&mut han_run, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn english_lowercased_prefix_tokens() {
        let toks: HashSet<_> = tokenize("Hello World").into_iter().collect();
        assert!(toks.contains("hello"));
        assert!(toks.contains("world"));
    }

    #[test]
    fn chinese_bigrams_and_unigrams() {
        let toks: HashSet<_> = tokenize("复制粘贴").into_iter().collect();
        assert!(toks.contains("复制")); // bigram
        assert!(toks.contains("制粘"));
        assert!(toks.contains("贴"));   // unigram
    }

    #[test]
    fn mixed_input() {
        let toks = tokenize("fastPaste 系统 2026");
        assert!(toks.contains(&"fastpaste".to_string()));
        assert!(toks.contains(&"系".to_string()));
        assert!(toks.contains(&"系统".to_string()));
        assert!(toks.contains(&"2026".to_string()));
    }

    #[test]
    fn single_han_char_hits_via_unigram() {
        // FR-Q02: 输入 1 个汉字也可命中
        let doc = tokenize("发票抬头");
        assert!(doc.contains(&"票".to_string()));
    }
}

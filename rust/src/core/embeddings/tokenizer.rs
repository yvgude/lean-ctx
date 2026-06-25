//! Minimal `WordPiece` tokenizer for BERT-style embedding models.
//!
//! Implements the standard BERT tokenization pipeline:
//! 1. Lowercase + accent stripping
//! 2. Whitespace + punctuation splitting
//! 3. `WordPiece` subword tokenization
//! 4. Special token insertion (`[CLS]`, `[SEP]`)
//!
//! Optimized for code search: handles camelCase, `snake_case`, and common
//! programming punctuation correctly.

use std::collections::HashMap;
use std::path::Path;

pub struct WordPieceTokenizer {
    vocab: HashMap<String, i32>,
    cls_id: i32,
    sep_id: i32,
    pad_id: i32,
    unk_id: i32,
    max_word_chars: usize,
}

#[derive(Debug, Clone)]
pub struct TokenizedInput {
    pub input_ids: Vec<i32>,
    pub attention_mask: Vec<i32>,
    pub token_type_ids: Vec<i32>,
}

impl TokenizedInput {
    /// Pad the input to a fixed length.
    pub fn pad_to(&mut self, target_len: usize, pad_id: i32) {
        while self.input_ids.len() < target_len {
            self.input_ids.push(pad_id);
            self.attention_mask.push(0);
            self.token_type_ids.push(0);
        }
    }
}

impl WordPieceTokenizer {
    /// Load vocabulary from a standard vocab.txt file (one token per line).
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Failed to read vocab file {}: {}", path.display(), e))?;
        Self::from_vocab_str(&content)
    }

    /// Build tokenizer from vocabulary string (one token per line).
    pub fn from_vocab_str(vocab_str: &str) -> anyhow::Result<Self> {
        let vocab: HashMap<String, i32> = vocab_str
            .lines()
            .enumerate()
            .map(|(i, line)| (line.to_string(), i as i32))
            .collect();

        let cls_id = *vocab
            .get("[CLS]")
            .ok_or_else(|| anyhow::anyhow!("Vocabulary missing [CLS] token"))?;
        let sep_id = *vocab
            .get("[SEP]")
            .ok_or_else(|| anyhow::anyhow!("Vocabulary missing [SEP] token"))?;
        let pad_id = *vocab
            .get("[PAD]")
            .ok_or_else(|| anyhow::anyhow!("Vocabulary missing [PAD] token"))?;
        let unk_id = *vocab
            .get("[UNK]")
            .ok_or_else(|| anyhow::anyhow!("Vocabulary missing [UNK] token"))?;

        Ok(Self {
            vocab,
            cls_id,
            sep_id,
            pad_id,
            unk_id,
            max_word_chars: 200,
        })
    }

    /// Encode text into token IDs with `[CLS]` prefix and `[SEP]` suffix.
    #[must_use]
    pub fn encode(&self, text: &str, max_len: usize) -> TokenizedInput {
        let words = self.pre_tokenize(text);
        let mut ids = vec![self.cls_id];

        for word in &words {
            if ids.len() >= max_len - 1 {
                break;
            }
            let subword_ids = self.wordpiece_encode(word);
            for id in subword_ids {
                if ids.len() >= max_len - 1 {
                    break;
                }
                ids.push(id);
            }
        }

        ids.push(self.sep_id);

        let len = ids.len();
        TokenizedInput {
            input_ids: ids,
            attention_mask: vec![1; len],
            token_type_ids: vec![0; len],
        }
    }

    #[must_use]
    pub fn pad_id(&self) -> i32 {
        self.pad_id
    }

    #[must_use]
    pub fn vocab_size(&self) -> usize {
        self.vocab.len()
    }

    /// Split text into word-level tokens.
    /// Handles: whitespace splitting, punctuation splitting,
    /// camelCase splitting, underscore-separated identifiers, then lowercase.
    fn pre_tokenize(&self, text: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut current = String::new();

        for ch in text.chars() {
            if ch.is_whitespace() {
                if !current.is_empty() {
                    words.extend(self.split_identifier(&current));
                    current.clear();
                }
            } else if is_bert_punctuation(ch) {
                if !current.is_empty() {
                    words.extend(self.split_identifier(&current));
                    current.clear();
                }
                words.push(ch.to_string());
            } else {
                current.push(ch);
            }
        }
        if !current.is_empty() {
            words.extend(self.split_identifier(&current));
        }

        words.iter().map(|w| w.to_lowercase()).collect()
    }

    /// Split programming identifiers (camelCase, `snake_case`) into subwords.
    /// Called BEFORE lowercasing so case boundaries are still visible.
    fn split_identifier(&self, word: &str) -> Vec<String> {
        let lower = word.to_lowercase();
        if self.vocab.contains_key(&lower) {
            return vec![word.to_string()];
        }

        let mut parts = Vec::new();
        let mut current = String::new();
        let chars: Vec<char> = word.chars().collect();

        for (i, &ch) in chars.iter().enumerate() {
            if ch == '_' || ch == '-' {
                if !current.is_empty() {
                    parts.push(current.clone());
                    current.clear();
                }
            } else if i > 0 && ch.is_ascii_uppercase() && chars[i - 1].is_ascii_lowercase() {
                if !current.is_empty() {
                    parts.push(current.clone());
                    current.clear();
                }
                current.push(ch);
            } else {
                current.push(ch);
            }
        }
        if !current.is_empty() {
            parts.push(current);
        }

        if parts.is_empty() {
            vec![word.to_string()]
        } else {
            parts
        }
    }

    /// Apply `WordPiece` algorithm to a single word.
    fn wordpiece_encode(&self, word: &str) -> Vec<i32> {
        if word.chars().count() > self.max_word_chars {
            return vec![self.unk_id];
        }

        let chars: Vec<char> = word.chars().collect();
        let mut tokens = Vec::new();
        let mut start = 0;

        while start < chars.len() {
            let mut end = chars.len();
            let mut matched = false;

            while start < end {
                let substr: String = chars[start..end].iter().collect();
                let candidate = if start > 0 {
                    format!("##{substr}")
                } else {
                    substr
                };

                if let Some(&id) = self.vocab.get(&candidate) {
                    tokens.push(id);
                    matched = true;
                    start = end;
                    break;
                }
                end -= 1;
            }

            if !matched {
                tokens.push(self.unk_id);
                start += 1;
            }
        }

        tokens
    }
}

/// BERT-style punctuation detection.
fn is_bert_punctuation(ch: char) -> bool {
    if ch.is_ascii() {
        matches!(
            ch,
            '!' | '"'
                | '#'
                | '$'
                | '%'
                | '&'
                | '\''
                | '('
                | ')'
                | '*'
                | '+'
                | ','
                | '-'
                | '.'
                | '/'
                | ':'
                | ';'
                | '<'
                | '='
                | '>'
                | '?'
                | '@'
                | '['
                | '\\'
                | ']'
                | '^'
                | '_'
                | '`'
                | '{'
                | '|'
                | '}'
                | '~'
        )
    } else {
        ch.is_ascii_punctuation()
    }
}

/// Wrapper for `HuggingFace` `tokenizer.json` files.
///
/// Parses the JSON to extract the vocabulary map, then delegates to
/// the existing `WordPieceTokenizer` for actual tokenization.
/// Supports both `WordPiece` and BPE vocab formats (both map tokens → IDs).
pub struct HfTokenizerWrapper {
    inner: WordPieceTokenizer,
}

impl HfTokenizerWrapper {
    /// Load from a `HuggingFace` `tokenizer.json` file.
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            anyhow::anyhow!("Failed to read tokenizer.json {}: {}", path.display(), e)
        })?;
        Self::from_json(&content)
    }

    fn from_json(json_str: &str) -> anyhow::Result<Self> {
        let parsed: serde_json::Value = serde_json::from_str(json_str)
            .map_err(|e| anyhow::anyhow!("Invalid tokenizer.json: {e}"))?;

        let vocab_obj = parsed
            .get("model")
            .and_then(|m| m.get("vocab"))
            .and_then(|v| v.as_object())
            .ok_or_else(|| anyhow::anyhow!("tokenizer.json missing model.vocab object"))?;

        let mut vocab_lines: Vec<(String, i32)> = vocab_obj
            .iter()
            .filter_map(|(token, id)| id.as_i64().map(|id| (token.clone(), id as i32)))
            .collect();
        vocab_lines.sort_by_key(|(_, id)| *id);

        // GL #397 / #498: BPE/RoBERTa-style tokenizer.json files ship
        // different special token names than BERT's `[CLS]`, `[SEP]`, etc.
        // Remap them here so the downstream `WordPieceTokenizer` can find
        // its expected sentinel tokens — the sort-order preserves the
        // correct original IDs.
        for (token, _) in &mut vocab_lines {
            let mapped: &str = match token.as_str() {
                "<s>" => "[CLS]",
                "</s>" => "[SEP]",
                "<pad>" => "[PAD]",
                "<unk>" => "[UNK]",
                // <mask> is unused by WordPieceTokenizer; map it for completeness.
                "<mask>" => "[MASK]",
                _ => continue,
            };
            *token = mapped.to_string();
        }

        let vocab_str: String = vocab_lines
            .into_iter()
            .map(|(token, _)| token)
            .collect::<Vec<_>>()
            .join("\n");

        let inner = WordPieceTokenizer::from_vocab_str(&vocab_str)?;
        Ok(Self { inner })
    }

    #[must_use]
    pub fn encode(&self, text: &str, max_len: usize) -> TokenizedInput {
        self.inner.encode(text, max_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_vocab() -> WordPieceTokenizer {
        let vocab = "[PAD]\n[UNK]\n[CLS]\n[SEP]\nhello\nworld\nfn\nvalidate\ntoken\n##s\n##ing\nauth\n##enticate\nuser\nhandle\nrequest\n##er\nprocess\ndata\n.\n,\n(\n)\n{";
        WordPieceTokenizer::from_vocab_str(vocab).unwrap()
    }

    #[test]
    fn encode_basic() {
        let tok = test_vocab();
        let input = tok.encode("hello world", 512);
        assert_eq!(input.input_ids[0], tok.cls_id);
        assert_eq!(*input.input_ids.last().unwrap(), tok.sep_id);
        assert!(input.input_ids.len() >= 4); // [CLS] hello world [SEP]
    }

    #[test]
    fn encode_attention_mask() {
        let tok = test_vocab();
        let input = tok.encode("hello", 512);
        assert!(input.attention_mask.iter().all(|&m| m == 1));
        assert_eq!(input.attention_mask.len(), input.input_ids.len());
    }

    #[test]
    fn encode_token_type_ids_are_zero() {
        let tok = test_vocab();
        let input = tok.encode("hello", 512);
        assert!(input.token_type_ids.iter().all(|&t| t == 0));
    }

    #[test]
    fn encode_respects_max_len() {
        let tok = test_vocab();
        let input = tok.encode("hello world hello world hello world", 6);
        assert!(input.input_ids.len() <= 6);
        assert_eq!(input.input_ids[0], tok.cls_id);
        assert_eq!(*input.input_ids.last().unwrap(), tok.sep_id);
    }

    #[test]
    fn wordpiece_subwords() {
        let tok = test_vocab();
        // "tokens" should split into "token" + "##s"
        let ids = tok.wordpiece_encode("tokens");
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0], *tok.vocab.get("token").unwrap());
        assert_eq!(ids[1], *tok.vocab.get("##s").unwrap());
    }

    #[test]
    fn wordpiece_unknown() {
        let tok = test_vocab();
        let ids = tok.wordpiece_encode("xyzzyplugh");
        assert!(ids.contains(&tok.unk_id));
    }

    #[test]
    fn pre_tokenize_camel_case() {
        let tok = test_vocab();
        let words = tok.pre_tokenize("handleRequest");
        assert!(words.contains(&"handle".to_string()));
        assert!(words.contains(&"request".to_string()));
    }

    #[test]
    fn pre_tokenize_snake_case() {
        let tok = test_vocab();
        let words = tok.pre_tokenize("validate_token");
        assert!(words.contains(&"validate".to_string()));
        assert!(words.contains(&"token".to_string()));
    }

    #[test]
    fn pre_tokenize_punctuation() {
        let tok = test_vocab();
        let words = tok.pre_tokenize("fn(x)");
        assert!(words.contains(&"fn".to_string()));
        assert!(words.contains(&"(".to_string()));
        assert!(words.contains(&")".to_string()));
    }

    #[test]
    fn pad_to_extends() {
        let tok = test_vocab();
        let mut input = tok.encode("hello", 512);
        let original_len = input.input_ids.len();
        input.pad_to(10, tok.pad_id);
        assert_eq!(input.input_ids.len(), 10);
        assert_eq!(input.attention_mask[original_len], 0);
    }

    #[test]
    fn vocab_size() {
        let tok = test_vocab();
        assert_eq!(tok.vocab_size(), 24);
    }

    #[test]
    fn empty_input() {
        let tok = test_vocab();
        let input = tok.encode("", 512);
        assert_eq!(input.input_ids.len(), 2); // [CLS] [SEP]
    }

    #[test]
    fn bert_punctuation_detection() {
        assert!(is_bert_punctuation('.'));
        assert!(is_bert_punctuation('('));
        assert!(is_bert_punctuation('{'));
        assert!(!is_bert_punctuation('a'));
        assert!(!is_bert_punctuation('0'));
    }

    #[test]
    fn hf_tokenizer_remaps_bpe_special_tokens() {
        // BPE/RoBERTa-style tokenizer.json with <s>/</s>/<pad>/<unk>
        let json = r#"{
            "version": "1.0",
            "model": {
                "type": "BPE",
                "vocab": {
                    "<s>": 0, "<pad>": 1, "</s>": 2, "<unk>": 3,
                    "hello": 4, "world": 5, "fn": 6
                }
            }
        }"#;
        let tok = HfTokenizerWrapper::from_json(json).unwrap();

        // After remapping: <s>→[CLS], <pad>→[PAD], </s>→[SEP], <unk>→[UNK]
        // IDs come from sort order (0,1,2,3,4,5,6) which matches original IDs.
        let input = tok.encode("hello world", 512);
        assert_eq!(
            input.input_ids[0], 0,
            "first token should be [CLS] (remapped from <s>)"
        );
        assert_eq!(
            *input.input_ids.last().unwrap(),
            2,
            "last token should be [SEP] (remapped from </s>)"
        );
        assert_eq!(input.input_ids.len(), 4); // [CLS] hello world [SEP]
    }

    #[test]
    fn hf_tokenizer_from_json() {
        let json = r#"{
            "version": "1.0",
            "model": {
                "type": "WordPiece",
                "vocab": {
                    "[PAD]": 0, "[UNK]": 1, "[CLS]": 2, "[SEP]": 3,
                    "hello": 4, "world": 5, "fn": 6
                }
            }
        }"#;
        let tok = HfTokenizerWrapper::from_json(json).unwrap();
        let input = tok.encode("hello world", 512);
        assert_eq!(input.input_ids[0], 2); // [CLS]
        assert_eq!(*input.input_ids.last().unwrap(), 3); // [SEP]
        assert!(input.input_ids.len() >= 4);
    }

    #[test]
    fn hf_tokenizer_invalid_json() {
        assert!(HfTokenizerWrapper::from_json("not json").is_err());
    }

    #[test]
    fn hf_tokenizer_missing_vocab() {
        let json = r#"{"model": {"type": "WordPiece"}}"#;
        assert!(HfTokenizerWrapper::from_json(json).is_err());
    }
}

//! Tokenizer for ASR
//!
//! Handles encoding/decoding between text and token IDs.
//! Supports BPE and character-level tokenization.

use crate::error::{AppError, Result};
use std::collections::HashMap;

/// Tokenizer for ASR models
pub struct Tokenizer {
    /// Vocabulary: token -> id
    vocab: HashMap<String, i32>,
    /// Reverse vocabulary: id -> token
    reverse_vocab: HashMap<i32, String>,
    /// Special tokens
    sos_id: i32,
    eos_id: i32,
    unk_id: i32,
    pad_id: i32,
}

impl Tokenizer {
    /// Create a new tokenizer from vocabulary file
    pub fn from_vocab(vocab_path: &std::path::Path) -> Result<Self> {
        let content = std::fs::read_to_string(vocab_path)
            .map_err(|e| AppError::Io(format!("Failed to read vocab file: {}", e)))?;

        let mut vocab = HashMap::new();
        let mut reverse_vocab = HashMap::new();
        let mut sos_id = -1i32;
        let mut eos_id = -1i32;
        let mut unk_id = 0i32;
        let mut pad_id = 0i32;

        // Parse vocab file (format: "token\tid" or JSON)
        for (idx, line) in content.lines().enumerate() {
            let token = if line.contains('\t') {
                // TSV format
                line.split('\t').next().unwrap_or("").to_string()
            } else if line.contains(':') && line.trim().starts_with('"') {
                // JSON-like format (simplified)
                line.split(':')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .trim_matches('"')
                    .to_string()
            } else {
                // Simple format: one token per line
                line.trim().to_string()
            };

            if !token.is_empty() {
                let id = idx as i32;
                vocab.insert(token.clone(), id);
                reverse_vocab.insert(id, token.clone());

                // Detect special tokens
                match token.as_str() {
                    "<s>" | "<sos>" | "[SOS]" => sos_id = id,
                    "</s>" | "<eos>" | "[EOS]" => eos_id = id,
                    "<unk>" | "[UNK]" => unk_id = id,
                    "<pad>" | "[PAD]" => pad_id = id,
                    _ => {}
                }
            }
        }

        // Use defaults if special tokens not found
        if sos_id < 0 {
            sos_id = vocab.get("<s>").copied().unwrap_or(1);
        }
        if eos_id < 0 {
            eos_id = vocab.get("</s>").copied().unwrap_or(2);
        }

        log::info!(
            "Loaded tokenizer with {} tokens, SOS={}, EOS={}, UNK={}, PAD={}",
            vocab.len(),
            sos_id,
            eos_id,
            unk_id,
            pad_id
        );

        Ok(Self {
            vocab,
            reverse_vocab,
            sos_id,
            eos_id,
            unk_id,
            pad_id,
        })
    }

    /// Encode text to token IDs
    pub fn encode(&self, text: &str) -> Vec<i32> {
        // Simple character-level encoding for now
        // For BPE, this would apply BPE merges
        let mut ids = vec![self.sos_id];

        for ch in text.chars() {
            let token = ch.to_string();
            let id = self.vocab.get(&token).copied().unwrap_or(self.unk_id);
            ids.push(id);
        }

        ids.push(self.eos_id);
        ids
    }

    /// Decode token IDs to text
    pub fn decode(&self, ids: &[i32], skip_special: bool) -> String {
        let mut text = String::new();

        for &id in ids {
            if skip_special && (id == self.sos_id || id == self.eos_id || id == self.pad_id) {
                continue;
            }

            if let Some(token) = self.reverse_vocab.get(&id) {
                text.push_str(token);
            } else {
                text.push('?');
            }
        }

        text
    }

    /// Get vocabulary size
    pub fn vocab_size(&self) -> usize {
        self.vocab.len()
    }

    /// Get SOS token ID
    pub fn sos_id(&self) -> i32 {
        self.sos_id
    }

    /// Get EOS token ID
    pub fn eos_id(&self) -> i32 {
        self.eos_id
    }

    /// Get UNK token ID
    pub fn unk_id(&self) -> i32 {
        self.unk_id
    }

    /// Get PAD token ID
    pub fn pad_id(&self) -> i32 {
        self.pad_id
    }

    /// Get token ID for a given token string
    pub fn token_to_id(&self, token: &str) -> i32 {
        self.vocab.get(token).copied().unwrap_or(self.unk_id)
    }

    /// Get token string for a given ID
    pub fn id_to_token(&self, id: i32) -> &str {
        self.reverse_vocab
            .get(&id)
            .map(|s| s.as_str())
            .unwrap_or("<unk>")
    }

    /// Create a simple character-level tokenizer for testing/fallback
    pub fn new_char_tokenizer() -> Result<Self> {
        let mut vocab = HashMap::new();
        let mut reverse_vocab = HashMap::new();

        // Add special tokens
        vocab.insert("<pad>".to_string(), 0);
        vocab.insert("<unk>".to_string(), 1);
        vocab.insert("<s>".to_string(), 2);
        vocab.insert("</s>".to_string(), 3);
        reverse_vocab.insert(0, "<pad>".to_string());
        reverse_vocab.insert(1, "<unk>".to_string());
        reverse_vocab.insert(2, "<s>".to_string());
        reverse_vocab.insert(3, "</s>".to_string());

        // Add ASCII printable characters
        for c in 32..=126u8 {
            let token = (c as char).to_string();
            let id = vocab.len() as i32;
            vocab.insert(token.clone(), id);
            reverse_vocab.insert(id, token);
        }

        // Add common CJK characters (simplified)
        for c in 0x4E00..=0x4FFFu32 {
            if let Some(ch) = std::char::from_u32(c) {
                let token = ch.to_string();
                let id = vocab.len() as i32;
                vocab.insert(token.clone(), id);
                reverse_vocab.insert(id, token);
            }
        }

        log::info!(
            "Created character-level tokenizer with {} tokens",
            vocab.len()
        );

        Ok(Self {
            vocab,
            reverse_vocab,
            sos_id: 2,
            eos_id: 3,
            unk_id: 1,
            pad_id: 0,
        })
    }
}

/// Simple character-level tokenizer for testing
pub struct CharTokenizer {
    chars: Vec<char>,
}

impl CharTokenizer {
    pub fn new() -> Self {
        // Basic CJK + ASCII characters
        let mut chars: Vec<char> = (0x4E00..=0x9FFF)
            .filter_map(|c| std::char::from_u32(c))
            .collect();
        chars.extend('a'..='z');
        chars.extend('A'..='Z');
        chars.extend('0'..='9');
        chars.extend([
            ' ', '.', ',', '!', '?', ':', ';', '-', '_', '(', ')', '[', ']', '{', '}', '"', '\'',
            '/',
        ]);

        Self { chars }
    }

    pub fn vocab_size(&self) -> usize {
        self.chars.len() + 4 // +4 for special tokens
    }

    pub fn encode(&self, text: &str) -> Vec<i32> {
        text.chars()
            .filter_map(|c| self.chars.iter().position(|&x| x == c))
            .map(|i| i as i32 + 4) // +4 for special tokens
            .collect()
    }

    pub fn decode(&self, ids: &[i32]) -> String {
        ids.iter()
            .filter(|&&id| id >= 4)
            .filter_map(|&id| self.chars.get((id - 4) as usize))
            .collect()
    }
}

impl Default for CharTokenizer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_char_tokenizer() {
        let tokenizer = CharTokenizer::new();

        let text = "hello";
        let ids = tokenizer.encode(text);
        let decoded = tokenizer.decode(&ids);

        assert_eq!(decoded, text);
    }

    #[test]
    fn test_tokenizer_special_tokens() {
        // Create a simple vocab file for testing
        let vocab_content = r#"<pad>
<unk>
<sos>
<eos>
a
b
c
"#;
        let temp_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(temp_file.path(), vocab_content).unwrap();

        let tokenizer = Tokenizer::from_vocab(temp_file.path()).unwrap();

        assert_eq!(tokenizer.pad_id(), 0);
        assert_eq!(tokenizer.unk_id(), 1);
        assert!(tokenizer.vocab_size() >= 7);
    }
}

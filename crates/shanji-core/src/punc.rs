use crate::error::{AppError, Result};
use ort::session::Session;
use ort::value::Tensor;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

const DEFAULT_SPLIT_SIZE: usize = 20;
const DEFAULT_CACHE_POP_TRIGGER_LIMIT: usize = 200;

#[derive(Debug)]
pub struct CtPuncModel {
    session: Session,
    token_to_id: HashMap<String, i32>,
    punc_list_zh: Vec<String>,
    punc_list_en: Vec<String>,
    period_index: usize,
    split_size: usize,
    cache_pop_trigger_limit: usize,
}

#[derive(Debug, Deserialize)]
struct PuncConfig {
    #[serde(default)]
    punc_list: Vec<String>,
}

impl CtPuncModel {
    pub fn new(model_dir: &Path) -> Result<Self> {
        let model_path = model_dir.join("model.onnx");
        let tokens_path = model_dir.join("tokens.txt");
        let json_path = model_dir.join("punc.json");
        let yaml_path = model_dir.join("punc.yaml");

        let session = Session::builder()
            .map_err(|e| AppError::Asr(format!("Failed to create punc session: {}", e)))?
            .commit_from_file(&model_path)
            .map_err(|e| AppError::Asr(format!("Failed to load punc model: {}", e)))?;

        let tokens = std::fs::read_to_string(&tokens_path).map_err(|e| {
            AppError::Asr(format!(
                "Failed to read punc tokens {}: {}",
                tokens_path.display(),
                e
            ))
        })?;
        let token_to_id = tokens
            .lines()
            .enumerate()
            .map(|(index, token)| {
                (
                    token.trim().trim_start_matches('\u{feff}').to_string(),
                    index as i32,
                )
            })
            .collect::<HashMap<_, _>>();

        let config = if json_path.exists() {
            let content = std::fs::read_to_string(&json_path).map_err(|e| {
                AppError::Asr(format!(
                    "Failed to read punc config {}: {}",
                    json_path.display(),
                    e
                ))
            })?;
            serde_json::from_str::<PuncConfig>(&content).map_err(|e| {
                AppError::Asr(format!(
                    "Failed to parse punc config {}: {}",
                    json_path.display(),
                    e
                ))
            })?
        } else {
            let content = std::fs::read_to_string(&yaml_path).map_err(|e| {
                AppError::Asr(format!(
                    "Failed to read punc config {}: {}",
                    yaml_path.display(),
                    e
                ))
            })?;
            serde_yaml::from_str::<PuncConfig>(&content).map_err(|e| {
                AppError::Asr(format!(
                    "Failed to parse punc config {}: {}",
                    yaml_path.display(),
                    e
                ))
            })?
        };

        if config.punc_list.is_empty() {
            return Err(AppError::Asr(
                "Punc config did not define any punctuation labels".to_string(),
            ));
        }

        let mut punc_list_zh = config
            .punc_list
            .iter()
            .map(|item| normalize_chinese_punctuation(item))
            .collect::<Vec<_>>();
        let punc_list_en = punc_list_zh
            .iter()
            .map(|item| normalize_english_punctuation(item))
            .collect::<Vec<_>>();
        let period_index = punc_list_zh
            .iter()
            .position(|item| item == "。")
            .ok_or_else(|| AppError::Asr("Punc config is missing period label".to_string()))?;

        for item in &mut punc_list_zh {
            *item = normalize_chinese_punctuation(item);
        }

        Ok(Self {
            session,
            token_to_id,
            punc_list_zh,
            punc_list_en,
            period_index,
            split_size: DEFAULT_SPLIT_SIZE,
            cache_pop_trigger_limit: DEFAULT_CACHE_POP_TRIGGER_LIMIT,
        })
    }

    pub fn restore(&mut self, text: &str, is_final: bool) -> Result<String> {
        let split_text = code_mix_split_words(text);
        if split_text.is_empty() {
            return Ok(String::new());
        }

        let split_ids = tokens_to_ids(&self.token_to_id, &split_text);
        let _mini_sentences = split_to_mini_sentences(&split_text, self.split_size);
        let mini_sentence_ids = split_to_mini_sentences(&split_ids, self.split_size);

        let mut cache_sentence_ids = Vec::<i32>::new();
        let mut punctuated_batches = Vec::<Vec<usize>>::new();

        for (batch_index, mini_sentence_id) in mini_sentence_ids.iter().enumerate() {
            let mut merged = cache_sentence_ids.clone();
            merged.extend_from_slice(mini_sentence_id);

            let normalized_ids = merged
                .iter()
                .map(|id| if *id == 0 { -1 } else { *id })
                .collect::<Vec<_>>();
            let mut punctuations = self.forward(&normalized_ids)?;

            if batch_index + 1 < mini_sentence_ids.len() {
                let mut sentence_end = None;
                let mut last_comma_index = None;
                for idx in (2..punctuations.len().saturating_sub(1)).rev() {
                    let symbol = self
                        .punc_list_zh
                        .get(punctuations[idx])
                        .map(String::as_str)
                        .unwrap_or("");
                    if matches!(symbol, "。" | "？") {
                        sentence_end = Some(idx);
                        break;
                    }
                    if last_comma_index.is_none() && symbol == "，" {
                        last_comma_index = Some(idx);
                    }
                }

                let sentence_end = sentence_end.or_else(|| {
                    (merged.len() > self.cache_pop_trigger_limit)
                        .then_some(last_comma_index)
                        .flatten()
                });

                if let Some(sentence_end) = sentence_end {
                    if self
                        .punc_list_zh
                        .get(punctuations[sentence_end])
                        .map(String::as_str)
                        == Some("，")
                    {
                        punctuations[sentence_end] = self.period_index;
                    }

                    cache_sentence_ids = merged[sentence_end + 1..].to_vec();
                    if sentence_end > 0 {
                        punctuated_batches.push(punctuations[..=sentence_end].to_vec());
                    }
                } else {
                    cache_sentence_ids = merged;
                }
            } else {
                if is_final {
                    if let Some(last) = punctuations.last_mut() {
                        let symbol = self
                            .punc_list_zh
                            .get(*last)
                            .map(String::as_str)
                            .unwrap_or("");
                        if matches!(symbol, "，" | "、") || !matches!(symbol, "。" | "？") {
                            *last = self.period_index;
                        }
                    }
                }
                punctuated_batches.push(punctuations);
            }
        }

        Ok(decode_punctuations(
            &split_text,
            &punctuated_batches,
            &self.punc_list_zh,
            &self.punc_list_en,
        ))
    }

    fn forward(&mut self, ids: &[i32]) -> Result<Vec<usize>> {
        let inputs_tensor =
            Tensor::from_array(([1usize, ids.len()], ids.to_vec().into_boxed_slice()))
                .map_err(|e| AppError::Asr(format!("Failed to build punc input tensor: {}", e)))?;
        let text_lengths_tensor =
            Tensor::from_array(([1usize], vec![ids.len() as i32].into_boxed_slice())).map_err(
                |e| AppError::Asr(format!("Failed to build punc text lengths tensor: {}", e)),
            )?;

        let outputs = self
            .session
            .run(ort::inputs! {
                "inputs" => inputs_tensor,
                "text_lengths" => text_lengths_tensor,
            })
            .map_err(|e| AppError::Asr(format!("Punc inference failed: {}", e)))?;

        let (_, logits) = outputs["logits"]
            .try_extract_tensor::<f32>()
            .map_err(|e| AppError::Asr(format!("Failed to extract punc logits: {}", e)))?;

        let class_count = self.punc_list_zh.len();
        let raw = logits.to_vec();
        let mut result = Vec::with_capacity(ids.len());
        for token_index in 0..ids.len() {
            let start = token_index * class_count;
            let end = start + class_count;
            let row = &raw[start..end.min(raw.len())];
            let mut best_index = 0usize;
            let mut best_score = f32::MIN;
            for (index, score) in row.iter().enumerate() {
                if *score >= best_score {
                    best_score = *score;
                    best_index = index;
                }
            }
            result.push(best_index);
        }

        Ok(result)
    }
}

fn normalize_chinese_punctuation(symbol: &str) -> String {
    symbol
        .replace(',', "，")
        .replace('?', "？")
        .replace('.', "。")
}

fn normalize_english_punctuation(symbol: &str) -> String {
    symbol
        .replace("，", ",")
        .replace('？', "?")
        .replace('。', ".")
}

fn code_mix_split_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();

    for segment in text.split_whitespace() {
        let mut current_ascii = String::new();
        for ch in segment.chars() {
            if ch.is_ascii() {
                current_ascii.push(ch);
            } else {
                if !current_ascii.is_empty() {
                    words.push(current_ascii.clone());
                    current_ascii.clear();
                }
                words.push(ch.to_string());
            }
        }

        if !current_ascii.is_empty() {
            current_ascii.push('▁');
            words.push(current_ascii);
        }
    }

    words
}

fn tokens_to_ids(token_to_id: &HashMap<String, i32>, split_text: &[String]) -> Vec<i32> {
    split_text
        .iter()
        .map(|token| {
            token_to_id
                .get(token.trim_end_matches('▁'))
                .copied()
                .unwrap_or(-1)
        })
        .collect()
}

fn split_to_mini_sentences<T: Clone>(words: &[T], word_limit: usize) -> Vec<Vec<T>> {
    if words.is_empty() {
        return Vec::new();
    }

    if words.len() <= word_limit {
        return vec![words.to_vec()];
    }

    let mut result = Vec::new();
    let mut start = 0usize;
    while start < words.len() {
        let end = (start + word_limit).min(words.len());
        result.push(words[start..end].to_vec());
        start = end;
    }
    result
}

fn decode_punctuations(
    split_text: &[String],
    punctuated_batches: &[Vec<usize>],
    punc_list_zh: &[String],
    punc_list_en: &[String],
) -> String {
    let mut word_index = 0usize;
    let mut out = String::new();

    for batch in punctuated_batches {
        for &punctuation_index in batch {
            if word_index >= split_text.len() {
                break;
            }

            let word = &split_text[word_index];
            out.push_str(word);
            word_index += 1;

            if punctuation_index <= 1 {
                continue;
            }

            let is_chinese_token = word
                .chars()
                .next()
                .map(|ch| !ch.is_ascii())
                .unwrap_or(false);
            let punctuation = if word.chars().count() > 1 || !is_chinese_token {
                punc_list_en
                    .get(punctuation_index)
                    .cloned()
                    .unwrap_or_default()
                    + " "
            } else {
                punc_list_zh
                    .get(punctuation_index)
                    .cloned()
                    .unwrap_or_default()
            };
            out.push_str(&punctuation);
        }
    }

    out.replace('▁', " ").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::{code_mix_split_words, decode_punctuations, split_to_mini_sentences};

    #[test]
    fn code_mix_split_words_keeps_ascii_runs() {
        let words = code_mix_split_words("hello世界 test");
        assert_eq!(words, vec!["hello", "世", "界", "test▁"]);
    }

    #[test]
    fn split_to_mini_sentences_chunks_evenly() {
        let parts = split_to_mini_sentences(&[1, 2, 3, 4, 5], 2);
        assert_eq!(parts, vec![vec![1, 2], vec![3, 4], vec![5]]);
    }

    #[test]
    fn decode_punctuations_uses_chinese_symbols() {
        let text = vec!["你".to_string(), "好".to_string()];
        let out = decode_punctuations(
            &text,
            &[vec![2, 3]],
            &[
                "<unk>".to_string(),
                "_".to_string(),
                "，".to_string(),
                "。".to_string(),
            ],
            &[
                "<unk>".to_string(),
                "_".to_string(),
                ",".to_string(),
                ".".to_string(),
            ],
        );
        assert_eq!(out, "你，好。");
    }
}

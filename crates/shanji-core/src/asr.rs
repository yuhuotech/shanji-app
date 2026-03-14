#[path = "asr/feature.rs"]
pub mod feature;
#[path = "asr/paraformer.rs"]
pub mod paraformer;
#[path = "asr/tokenizer.rs"]
pub mod tokenizer;

use crate::error::{AppError, Result};
use feature::{FBankConfig, FBankExtractor};
use ort::session::Session;
use paraformer::{StreamingParaformer, WholeModelParaformer};
use std::path::{Path, PathBuf};
use tokenizer::Tokenizer;

enum AsrBackend {
    Streaming(StreamingParaformer),
    Whole(WholeModelParaformer),
}

impl AsrBackend {
    fn process_features(&mut self, features: &[Vec<f32>]) -> Result<Option<Vec<i32>>> {
        match self {
            AsrBackend::Streaming(streaming) => streaming.process_features(features),
            AsrBackend::Whole(model) => {
                if features.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(model.infer(features)?))
                }
            }
        }
    }

    fn finalize(&mut self, remaining_features: Option<&[Vec<f32>]>) -> Result<Vec<i32>> {
        match self {
            AsrBackend::Streaming(streaming) => {
                if let Some(features) = remaining_features {
                    let _ = streaming.process_features(features)?;
                }
                streaming.finalize()
            }
            AsrBackend::Whole(model) => {
                if let Some(features) = remaining_features {
                    model.infer(features)
                } else {
                    Ok(Vec::new())
                }
            }
        }
    }

    fn reset(&mut self) {
        match self {
            AsrBackend::Streaming(streaming) => streaming.reset(),
            AsrBackend::Whole(model) => model.reset(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AsrConfig {
    pub chunk_size: usize,
    pub left_context: usize,
    pub right_context: usize,
    pub insert_punct: bool,
    pub punct_style: String,
    pub hotwords: Vec<(String, i32)>,
}

impl Default for AsrConfig {
    fn default() -> Self {
        Self {
            chunk_size: 67,
            left_context: 6,
            right_context: 0,
            insert_punct: true,
            punct_style: "zh".to_string(),
            hotwords: Vec::new(),
        }
    }
}

pub struct AsrEngine {
    tokenizer: Option<Tokenizer>,
    feature_extractor: FBankExtractor,
    config: AsrConfig,
    backend: Option<AsrBackend>,
    sample_buffer: Vec<f32>,
    partial_result: String,
}

impl AsrEngine {
    pub fn new(config: AsrConfig) -> Result<Self> {
        let feature_extractor = FBankExtractor::new(FBankConfig::default())?;

        Ok(Self {
            tokenizer: None,
            feature_extractor,
            config,
            backend: None,
            sample_buffer: Vec::new(),
            partial_result: String::new(),
        })
    }

    pub fn load_model(&mut self, model_dir: &Path) -> Result<()> {
        let backend = {
            let model_path = model_dir.join("model.onnx");
            let model_quant_path = model_dir.join("model_quant.onnx");

            let model_file = if model_path.exists() {
                model_path
            } else if model_quant_path.exists() {
                model_quant_path
            } else {
                PathBuf::new()
            };

            if !model_file.as_os_str().is_empty() {
                let session = Session::builder()
                    .map_err(|e| AppError::Asr(format!("Failed to create model session: {}", e)))?
                    .commit_from_file(&model_file)
                    .map_err(|e| AppError::Asr(format!("Failed to load model: {}", e)))?;
                AsrBackend::Whole(WholeModelParaformer::new(session))
            } else {
                let encoder_path = model_dir.join("encoder.onnx");
                let encoder = if encoder_path.exists() {
                    Session::builder()
                        .map_err(|e| {
                            AppError::Asr(format!("Failed to create encoder session: {}", e))
                        })?
                        .commit_from_file(&encoder_path)
                        .map_err(|e| AppError::Asr(format!("Failed to load encoder: {}", e)))?
                } else {
                    return Err(AppError::Asr(format!(
                        "Model not found, expected model.onnx, model_quant.onnx, or encoder.onnx in {:?}",
                        model_dir
                    )));
                };

                let decoder_path = model_dir.join("decoder.onnx");
                let decoder = if decoder_path.exists() {
                    Some(
                        Session::builder()
                            .map_err(|e| {
                                AppError::Asr(format!("Failed to create decoder session: {}", e))
                            })?
                            .commit_from_file(&decoder_path)
                            .map_err(|e| AppError::Asr(format!("Failed to load decoder: {}", e)))?,
                    )
                } else {
                    None
                };

                AsrBackend::Streaming(StreamingParaformer::new(
                    encoder,
                    decoder,
                    self.config.chunk_size,
                ))
            }
        };

        let vocab_path = model_dir.join("vocab.txt");
        let vocab_json_path = model_dir.join("vocab.json");

        let tokenizer = if vocab_path.exists() {
            Tokenizer::from_vocab(&vocab_path)?
        } else if vocab_json_path.exists() {
            Tokenizer::from_vocab(&vocab_json_path)?
        } else {
            Tokenizer::new_char_tokenizer()?
        };

        self.backend = Some(backend);
        self.tokenizer = Some(tokenizer);
        Ok(())
    }

    pub fn is_loaded(&self) -> bool {
        self.backend.is_some() && self.tokenizer.is_some()
    }

    pub fn process_chunk(&mut self, audio: &[f32]) -> Result<Option<String>> {
        if !self.is_loaded() {
            return Err(AppError::Asr("Model not loaded".to_string()));
        }

        self.sample_buffer.extend_from_slice(audio);
        let samples_per_chunk = 16_000;
        if self.sample_buffer.len() >= samples_per_chunk {
            let chunk = self.sample_buffer[..samples_per_chunk].to_vec();
            self.sample_buffer = self.sample_buffer[samples_per_chunk..].to_vec();

            let features = self.feature_extractor.extract(&chunk)?;
            if !features.is_empty() {
                if let Some(ref mut backend) = self.backend {
                    if let Some(tokens) = backend.process_features(&features)? {
                        if let Some(ref tokenizer) = self.tokenizer {
                            let text = tokenizer.decode(&tokens, true);
                            self.partial_result.push_str(&text);
                            let processed = post_process_text(&self.partial_result);
                            return Ok(Some(processed));
                        }
                    }
                }
            }
        }

        Ok(None)
    }

    pub fn finalize(&mut self) -> Result<String> {
        if !self.is_loaded() {
            return Err(AppError::Asr("Model not loaded".to_string()));
        }

        let remaining_features = if !self.sample_buffer.is_empty() {
            Some(self.feature_extractor.extract(&self.sample_buffer)?)
        } else {
            None
        };

        let final_tokens = if let Some(ref mut backend) = self.backend {
            backend.finalize(remaining_features.as_deref())?
        } else {
            Vec::new()
        };

        let result = if let Some(ref tokenizer) = self.tokenizer {
            let final_text = tokenizer.decode(&final_tokens, true);
            let raw_text = format!("{}{}", self.partial_result, final_text);
            post_process_text(&raw_text)
        } else {
            post_process_text(&self.partial_result)
        };

        self.reset();
        Ok(result)
    }

    pub fn reset(&mut self) {
        self.sample_buffer.clear();
        self.partial_result.clear();

        if let Some(ref mut backend) = self.backend {
            backend.reset();
        }
    }
}

impl Default for AsrEngine {
    fn default() -> Self {
        Self::new(AsrConfig::default()).expect("Failed to create default ASR engine")
    }
}

pub fn post_process_text(text: &str) -> String {
    const FILLER_WORDS: &[&str] = &["嗯", "啊", "哦", "呃", "哎", "呢", "吧", "嘛"];

    let mut result = text.to_string();
    for filler in FILLER_WORDS {
        result = result.replace(filler, "");
    }

    let corrections = [
        ("测是", "测试"),
        ("那试", "测试"),
        ("侧是", "测试"),
        ("册是", "测试"),
    ];
    for (wrong, correct) in corrections {
        result = result.replace(wrong, correct);
    }

    let mut deduped = String::new();
    let mut prev_char = '\0';
    let mut repeat_count = 0;

    for ch in result.chars() {
        if ch == prev_char {
            repeat_count += 1;
            if repeat_count <= 1 {
                deduped.push(ch);
            }
        } else {
            repeat_count = 0;
            deduped.push(ch);
            prev_char = ch;
        }
    }

    deduped.trim().to_string()
}

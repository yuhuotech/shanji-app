#[path = "asr/feature.rs"]
pub mod feature;
#[path = "asr/paraformer.rs"]
pub mod paraformer;
#[path = "asr/tokenizer.rs"]
pub mod tokenizer;

use crate::error::{AppError, Result};
use crate::model::{ModelBackend, ResolvedModelLayout};
use crate::text_processing::normalize_transcript;
use feature::{FBankConfig, FBankExtractor};
use ort::session::Session;
use paraformer::{StreamingParaformer, WholeModelParaformer};
use serde::Deserialize;
use tokenizer::Tokenizer;

enum AsrBackend {
    Streaming(StreamingParaformer),
    Whole(WholeModelParaformer),
}

impl AsrBackend {
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
    config: AsrConfig,
    tokenizer: Option<Tokenizer>,
    feature_extractor: FBankExtractor,
    backend: Option<AsrBackend>,
    sample_buffer: Vec<f32>,
    whole_audio_buffer: Vec<f32>,
    partial_result: String,
    samples_per_chunk: usize,
}

impl AsrEngine {
    pub fn new(_config: AsrConfig) -> Result<Self> {
        let feature_extractor = FBankExtractor::new(FBankConfig::default())?;

        Ok(Self {
            config: _config,
            tokenizer: None,
            feature_extractor,
            backend: None,
            sample_buffer: Vec::new(),
            whole_audio_buffer: Vec::new(),
            partial_result: String::new(),
            samples_per_chunk: 8_000,
        })
    }

    pub fn load_model(&mut self, layout: &ResolvedModelLayout) -> Result<()> {
        let runtime_config = ModelRuntimeConfig::from_layout(layout);
        let backend = match layout.backend {
            ModelBackend::Whole => {
                let model_file = layout
                    .model_path
                    .clone()
                    .or_else(|| layout.model_quant_path.clone())
                    .ok_or_else(|| {
                        AppError::Asr("Whole model artifact not configured".to_string())
                    })?;

                let session = Session::builder()
                    .map_err(|e| AppError::Asr(format!("Failed to create model session: {}", e)))?
                    .commit_from_file(&model_file)
                    .map_err(|e| AppError::Asr(format!("Failed to load model: {}", e)))?;
                AsrBackend::Whole(WholeModelParaformer::new(session))
            }
            ModelBackend::Streaming => {
                let encoder_path = layout.encoder_path.clone().ok_or_else(|| {
                    AppError::Asr("Streaming encoder artifact not configured".to_string())
                })?;
                let encoder = Session::builder()
                    .map_err(|e| AppError::Asr(format!("Failed to create encoder session: {}", e)))?
                    .commit_from_file(&encoder_path)
                    .map_err(|e| AppError::Asr(format!("Failed to load encoder: {}", e)))?;

                let decoder_path = layout.decoder_path.clone().ok_or_else(|| {
                    AppError::Asr("Streaming decoder artifact not configured".to_string())
                })?;
                let decoder = Some(
                    Session::builder()
                        .map_err(|e| {
                            AppError::Asr(format!("Failed to create decoder session: {}", e))
                        })?
                        .commit_from_file(&decoder_path)
                        .map_err(|e| AppError::Asr(format!("Failed to load decoder: {}", e)))?,
                );

                AsrBackend::Streaming(StreamingParaformer::new(
                    encoder,
                    decoder,
                    runtime_config.streaming_chunk_size,
                    runtime_config.predictor_tail_threshold,
                )?)
            }
        };

        let tokenizer = Tokenizer::from_vocab(&layout.vocab_path)?;

        let feature_config = match &backend {
            AsrBackend::Whole(model) => {
                if model.expected_feat_dim() == runtime_config.feature_config.num_mel_bins {
                    FBankConfig {
                        lfr: None,
                        ..runtime_config.feature_config.clone()
                    }
                } else {
                    runtime_config.feature_config.clone()
                }
            }
            AsrBackend::Streaming(_) => runtime_config.feature_config.clone(),
        };

        self.feature_extractor = FBankExtractor::new(feature_config)?;
        self.samples_per_chunk = runtime_config.samples_per_chunk;

        log::info!(
            "ASR model loaded: backend={:?}, model_dir={}, config={:?}, samples_per_chunk={}, streaming_chunk_size={}, feature_bins={}, lfr={:?}",
            layout.backend,
            layout.model_dir.display(),
            layout.config_path.as_ref().map(|path| path.display().to_string()),
            self.samples_per_chunk,
            runtime_config.streaming_chunk_size,
            runtime_config.feature_config.num_mel_bins,
            runtime_config.feature_config.lfr
        );

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
        if self.sample_buffer.len() < self.samples_per_chunk {
            return Ok(None);
        }

        let tokenizer = self
            .tokenizer
            .as_ref()
            .ok_or_else(|| AppError::Asr("Tokenizer not loaded".to_string()))?;

        match self.backend.as_mut() {
            Some(AsrBackend::Streaming(streaming)) => {
                let chunk = self.sample_buffer[..self.samples_per_chunk].to_vec();
                self.sample_buffer = self.sample_buffer[self.samples_per_chunk..].to_vec();

                let features = self.feature_extractor.extract(&chunk)?;
                if features.is_empty() {
                    log::info!(
                        "Streaming ASR chunk skipped: empty features for {} samples",
                        chunk.len()
                    );
                    return Ok(None);
                }

                log::info!(
                    "Streaming ASR chunk ready: samples={}, feature_frames={}, buffered_remaining={}",
                    chunk.len(),
                    features.len(),
                    self.sample_buffer.len()
                );

                if let Some(tokens) = streaming.process_features(&features)? {
                    let text = tokenizer.decode(&tokens, true);
                    self.partial_result.push_str(&text);
                    let processed = self.render_transcript(&self.partial_result, false);
                    log::info!(
                        "Streaming ASR partial: tokens={}, raw='{}', processed='{}'",
                        tokens.len(),
                        text,
                        processed
                    );
                    return Ok(Some(processed));
                }

                log::info!(
                    "Streaming ASR chunk produced no partial output (feature_frames={})",
                    features.len()
                );

                Ok(None)
            }
            Some(AsrBackend::Whole(model)) => {
                self.whole_audio_buffer
                    .extend_from_slice(&self.sample_buffer);
                self.sample_buffer.clear();

                let features = self.feature_extractor.extract(&self.whole_audio_buffer)?;
                if features.is_empty() {
                    return Ok(None);
                }

                let tokens = model.infer(&features)?;
                let processed = self.render_transcript(&tokenizer.decode(&tokens, true), false);
                if processed.is_empty() {
                    log::info!(
                        "Whole-model ASR partial empty: audio_samples={}, feature_frames={}",
                        self.whole_audio_buffer.len(),
                        features.len()
                    );
                    return Ok(None);
                }

                // Whole-model partials should reflect the current best hypothesis,
                // not append independent chunk decodes together.
                self.partial_result = tokenizer.decode(&tokens, true);
                let processed = self.render_transcript(&self.partial_result, false);
                log::info!(
                    "Whole-model ASR partial: tokens={}, processed='{}'",
                    tokens.len(),
                    processed
                );
                Ok(Some(processed))
            }
            None => Err(AppError::Asr("Model not loaded".to_string())),
        }
    }

    pub fn finalize(&mut self) -> Result<String> {
        if !self.is_loaded() {
            return Err(AppError::Asr("Model not loaded".to_string()));
        }

        let tokenizer = self
            .tokenizer
            .as_ref()
            .ok_or_else(|| AppError::Asr("Tokenizer not loaded".to_string()))?;

        let result = match self.backend.as_mut() {
            Some(AsrBackend::Streaming(streaming)) => {
                let remaining_features = if !self.sample_buffer.is_empty() {
                    Some(self.feature_extractor.extract(&self.sample_buffer)?)
                } else {
                    None
                };
                if let Some(features) = remaining_features.as_deref() {
                    log::info!(
                        "Streaming ASR finalize with remaining buffer: samples={}, feature_frames={}",
                        self.sample_buffer.len(),
                        features.len()
                    );
                    let _ = streaming.process_features(features)?;
                }
                let final_tokens = streaming.finalize()?;
                let final_text = tokenizer.decode(&final_tokens, true);
                let raw_text = format!("{}{}", self.partial_result, final_text);
                log::info!(
                    "Streaming ASR final: tokens={}, raw='{}'",
                    final_tokens.len(),
                    raw_text
                );
                self.render_transcript(&raw_text, false)
            }
            Some(AsrBackend::Whole(model)) => {
                if !self.sample_buffer.is_empty() {
                    self.whole_audio_buffer
                        .extend_from_slice(&self.sample_buffer);
                    self.sample_buffer.clear();
                }

                if self.whole_audio_buffer.is_empty() {
                    self.render_transcript(&self.partial_result, false)
                } else {
                    let features = self.feature_extractor.extract(&self.whole_audio_buffer)?;
                    if features.is_empty() {
                        self.render_transcript(&self.partial_result, false)
                    } else {
                        let final_tokens = model.infer(&features)?;
                        let final_text = tokenizer.decode(&final_tokens, true);
                        let best_effort = if final_text.is_empty() {
                            self.partial_result.clone()
                        } else {
                            final_text
                        };
                        log::info!("Whole-model ASR final: '{}'", best_effort);
                        self.render_transcript(&best_effort, false)
                    }
                }
            }
            None => return Err(AppError::Asr("Model not loaded".to_string())),
        };

        self.reset();
        Ok(result)
    }

    pub fn reset(&mut self) {
        self.sample_buffer.clear();
        self.whole_audio_buffer.clear();
        self.partial_result.clear();

        if let Some(ref mut backend) = self.backend {
            backend.reset();
        }
    }

    fn render_transcript(&self, text: &str, is_final: bool) -> String {
        let _ = is_final;
        let _ = &self.config;
        normalize_transcript(text)
    }
}

#[derive(Debug, Clone)]
struct ModelRuntimeConfig {
    feature_config: FBankConfig,
    streaming_chunk_size: usize,
    samples_per_chunk: usize,
    predictor_tail_threshold: f32,
}

impl Default for ModelRuntimeConfig {
    fn default() -> Self {
        let feature_config = FBankConfig::default();
        Self {
            samples_per_chunk: 8_000,
            streaming_chunk_size: 67,
            predictor_tail_threshold: 0.45,
            feature_config,
        }
    }
}

impl ModelRuntimeConfig {
    fn from_layout(layout: &ResolvedModelLayout) -> Self {
        let mut config = Self::default();

        if let Some(model_yaml) = load_model_yaml_config(layout.config_path.as_deref()) {
            if let Some(sample_rate) = model_yaml
                .frontend_conf
                .as_ref()
                .and_then(|frontend| frontend.fs)
            {
                config.feature_config.sample_rate = sample_rate;
            }
            if let Some(frame_length_ms) = model_yaml
                .frontend_conf
                .as_ref()
                .and_then(|frontend| frontend.frame_length)
            {
                config.feature_config.frame_length_ms = frame_length_ms;
            }
            if let Some(frame_shift_ms) = model_yaml
                .frontend_conf
                .as_ref()
                .and_then(|frontend| frontend.frame_shift)
            {
                config.feature_config.frame_shift_ms = frame_shift_ms;
            }
            if let Some(num_mel_bins) = model_yaml
                .frontend_conf
                .as_ref()
                .and_then(|frontend| frontend.n_mels)
            {
                config.feature_config.num_mel_bins = num_mel_bins;
            }
            if let Some(lfr) = model_yaml
                .frontend_conf
                .as_ref()
                .and_then(|frontend| frontend.lfr())
            {
                config.feature_config.lfr = Some(lfr);
            }
            if let Some(chunk_size) = model_yaml
                .encoder_conf
                .as_ref()
                .and_then(|encoder| encoder.chunk_size_value())
            {
                config.streaming_chunk_size = chunk_size;
            }
            if let Some(tail_threshold) = model_yaml
                .predictor_conf
                .as_ref()
                .and_then(|predictor| predictor.tail_threshold)
            {
                config.predictor_tail_threshold = tail_threshold;
            }
        }

        if layout.backend == ModelBackend::Streaming {
            config.samples_per_chunk =
                streaming_samples_for_chunk(&config.feature_config, config.streaming_chunk_size);
        }

        config
    }
}

#[derive(Debug, Deserialize)]
struct ModelYamlConfig {
    #[serde(default)]
    frontend_conf: Option<ModelYamlFrontendConf>,
    #[serde(default)]
    encoder_conf: Option<ModelYamlEncoderConf>,
    #[serde(default)]
    predictor_conf: Option<ModelYamlPredictorConf>,
}

#[derive(Debug, Deserialize)]
struct ModelYamlFrontendConf {
    #[serde(default)]
    fs: Option<usize>,
    #[serde(default)]
    frame_length: Option<usize>,
    #[serde(default)]
    frame_shift: Option<usize>,
    #[serde(default)]
    n_mels: Option<usize>,
    #[serde(default)]
    lfr_m: Option<usize>,
    #[serde(default)]
    lfr_n: Option<usize>,
}

impl ModelYamlFrontendConf {
    fn lfr(&self) -> Option<(usize, usize)> {
        Some((self.lfr_m?, self.lfr_n?))
    }
}

#[derive(Debug, Deserialize)]
struct ModelYamlEncoderConf {
    #[serde(default)]
    chunk_size: Vec<usize>,
}

#[derive(Debug, Deserialize)]
struct ModelYamlPredictorConf {
    #[serde(default)]
    tail_threshold: Option<f32>,
}

impl ModelYamlEncoderConf {
    fn chunk_size_value(&self) -> Option<usize> {
        self.chunk_size
            .iter()
            .copied()
            .max()
            .filter(|value| *value > 0)
    }
}

fn load_model_yaml_config(path: Option<&std::path::Path>) -> Option<ModelYamlConfig> {
    let path = path?;
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) => {
            log::warn!(
                "Failed to read ASR model config {}: {}",
                path.display(),
                err
            );
            return None;
        }
    };

    match serde_yaml::from_str::<ModelYamlConfig>(&content) {
        Ok(config) => Some(config),
        Err(err) => {
            log::warn!(
                "Failed to parse ASR model config {}: {}",
                path.display(),
                err
            );
            None
        }
    }
}

fn streaming_samples_for_chunk(feature_config: &FBankConfig, chunk_size: usize) -> usize {
    let frame_length = feature_config.sample_rate * feature_config.frame_length_ms / 1000;
    let frame_shift = feature_config.sample_rate * feature_config.frame_shift_ms / 1000;
    let required_frames = if let Some((lfr_m, lfr_n)) = feature_config.lfr {
        lfr_m + chunk_size.saturating_sub(1) * lfr_n
    } else {
        chunk_size.max(1)
    };

    let required_samples = frame_length + required_frames.saturating_sub(1) * frame_shift;
    required_samples.max(frame_length)
}

impl Default for AsrEngine {
    fn default() -> Self {
        Self::new(AsrConfig::default()).expect("Failed to create default ASR engine")
    }
}

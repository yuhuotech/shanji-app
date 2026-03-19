#[path = "asr/feature.rs"]
pub mod feature;
#[path = "asr/paraformer.rs"]
pub mod paraformer;
#[path = "asr/tokenizer.rs"]
pub mod tokenizer;

use crate::error::{AppError, Result};
use crate::hotwords::Hotword;
use crate::model::{ModelBackend, ResolvedModelLayout};
use crate::text_processing::normalize_transcript_with_hotwords;
use feature::{apply_lfr, CmvnStats, FBankConfig, FBankExtractor};
use ort::session::Session;
use paraformer::{StreamingParaformer, WholeModelParaformer};
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
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
    pub hotwords: Vec<Hotword>,
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
    samples_per_window: usize,
    samples_per_step: usize,
    /// Accumulated raw 80-dim FBank frames for streaming LFR continuity
    raw_frame_buffer: Vec<Vec<f32>>,
    /// Number of LFR frames already emitted from raw_frame_buffer
    lfr_frames_emitted: usize,
    /// Whether this is the first streaming chunk (for overlap dedup)
    is_first_chunk: bool,
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
            samples_per_window: 8_000,
            samples_per_step: 8_000,
            raw_frame_buffer: Vec::new(),
            lfr_frames_emitted: 0,
            is_first_chunk: true,
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
                    runtime_config.encoder_left_context,
                    runtime_config.predictor_tail_threshold,
                )?)
            }
            ModelBackend::Auxiliary => {
                return Err(AppError::Asr(
                    "Auxiliary models cannot be loaded as ASR backends".to_string(),
                ));
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

        let mut feature_extractor = FBankExtractor::new(feature_config)?;

        // Load CMVN normalization from am.mvn if available
        let cmvn_loaded;
        if let Some(ref mvn_path) = layout.mean_variance_path {
            log::info!(
                "[DIAG] CMVN file path: {}, exists={}",
                mvn_path.display(),
                mvn_path.exists()
            );
            match CmvnStats::from_file(mvn_path) {
                Ok(cmvn) => {
                    log::info!("[DIAG] CMVN loaded OK: dim={}", cmvn.shift.len());
                    feature_extractor.set_cmvn(cmvn);
                    cmvn_loaded = true;
                }
                Err(err) => {
                    log::warn!(
                        "[DIAG] CMVN load FAILED from {}: {}",
                        mvn_path.display(),
                        err
                    );
                    cmvn_loaded = false;
                }
            }
        } else {
            log::warn!("[DIAG] CMVN path is None — mean_variance_path not in model layout");
            cmvn_loaded = false;
        }

        self.feature_extractor = feature_extractor;
        self.samples_per_window = runtime_config.samples_per_window;
        self.samples_per_step = runtime_config.samples_per_step;

        log::info!(
            "ASR model loaded: backend={:?}, model_dir={}, config={:?}, samples_per_window={}, samples_per_step={}, streaming_chunk_size={}, feature_bins={}, lfr={:?}, cmvn_loaded={}",
            layout.backend,
            layout.model_dir.display(),
            layout.config_path.as_ref().map(|path| path.display().to_string()),
            self.samples_per_window,
            self.samples_per_step,
            runtime_config.streaming_chunk_size,
            runtime_config.feature_config.num_mel_bins,
            runtime_config.feature_config.lfr,
            cmvn_loaded
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
        if self.sample_buffer.len() < self.samples_per_window {
            return Ok(None);
        }

        let tokenizer = self
            .tokenizer
            .as_ref()
            .ok_or_else(|| AppError::Asr("Tokenizer not loaded".to_string()))?;
        let hotwords = self.config.hotwords.clone();

        match self.backend.as_mut() {
            Some(AsrBackend::Streaming(streaming)) => {
                let mut latest_processed = None;

                while self.sample_buffer.len() >= self.samples_per_window {
                    let chunk = self.sample_buffer[..self.samples_per_window].to_vec();
                    let drain_len = self.samples_per_step.min(self.sample_buffer.len());
                    self.sample_buffer.drain(..drain_len);

                    // Extract raw 80-dim FBank frames (no LFR, no CMVN)
                    let raw_frames = self.feature_extractor.extract_fbank_only(&chunk)?;
                    if raw_frames.is_empty() {
                        continue;
                    }

                    // Handle overlap dedup: subsequent chunks share ~1 frame with previous
                    let new_frames = if self.is_first_chunk {
                        self.is_first_chunk = false;
                        raw_frames
                    } else {
                        // Skip the first frame which overlaps with the last of previous chunk
                        if raw_frames.len() > 1 {
                            raw_frames[1..].to_vec()
                        } else {
                            continue;
                        }
                    };

                    self.raw_frame_buffer.extend(new_frames);

                    // Apply LFR on the entire accumulated raw frame buffer
                    let lfr_config = self.feature_extractor.lfr_config();
                    let all_lfr = if let Some((m, n)) = lfr_config {
                        apply_lfr(&self.raw_frame_buffer, m, n)
                    } else {
                        self.raw_frame_buffer.clone()
                    };

                    // Only take newly produced LFR frames
                    if all_lfr.len() <= self.lfr_frames_emitted {
                        continue;
                    }
                    let mut new_lfr_frames = all_lfr[self.lfr_frames_emitted..].to_vec();
                    self.lfr_frames_emitted = all_lfr.len();

                    // Apply CMVN on new LFR frames
                    if let Some(cmvn) = self.feature_extractor.cmvn() {
                        cmvn.apply(&mut new_lfr_frames);
                    }

                    // Diagnostic: log once
                    static LOGGED_STREAMING: AtomicBool = AtomicBool::new(false);
                    if !new_lfr_frames.is_empty() && !LOGGED_STREAMING.swap(true, Ordering::Relaxed)
                    {
                        let f = &new_lfr_frames[0];
                        log::info!(
                            "[DIAG] streaming incremental LFR: raw_buffer={}, lfr_total={}, new_lfr={}, dim={}, first5={:?}",
                            self.raw_frame_buffer.len(),
                            self.lfr_frames_emitted,
                            new_lfr_frames.len(),
                            f.len(),
                            &f[..5.min(f.len())],
                        );
                    }

                    log::info!(
                        "Streaming ASR chunk ready: samples={}, raw_frames_total={}, new_lfr_frames={}, drain_len={}, buffered_remaining={}",
                        chunk.len(),
                        self.raw_frame_buffer.len(),
                        new_lfr_frames.len(),
                        drain_len,
                        self.sample_buffer.len()
                    );

                    if let Some(tokens) = streaming.process_features(&new_lfr_frames)? {
                        let text = tokenizer.decode(&tokens, true);
                        append_incremental_text(&mut self.partial_result, &text);
                        let processed =
                            normalize_transcript_with_hotwords(&self.partial_result, &hotwords);
                        log::info!(
                            "Streaming ASR partial: tokens={}, raw='{}', processed='{}'",
                            tokens.len(),
                            text,
                            processed
                        );
                        latest_processed = Some(processed);
                    }
                }

                if latest_processed.is_none() {
                    log::debug!(
                        "Streaming ASR buffered without partial output: buffered_remaining={}",
                        self.sample_buffer.len()
                    );
                }

                Ok(latest_processed)
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
                let processed =
                    normalize_transcript_with_hotwords(&tokenizer.decode(&tokens, true), &hotwords);
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
                let processed = normalize_transcript_with_hotwords(&self.partial_result, &hotwords);
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
        let hotwords = self.config.hotwords.clone();

        let result = match self.backend.as_mut() {
            Some(AsrBackend::Streaming(streaming)) => {
                // Flush remaining audio samples through incremental LFR pipeline
                if !self.sample_buffer.is_empty() {
                    let raw_frames = self
                        .feature_extractor
                        .extract_fbank_only(&self.sample_buffer)?;
                    if !raw_frames.is_empty() {
                        let new_frames = if self.is_first_chunk {
                            raw_frames
                        } else if raw_frames.len() > 1 {
                            raw_frames[1..].to_vec()
                        } else {
                            raw_frames
                        };
                        self.raw_frame_buffer.extend(new_frames);
                    }

                    let lfr_config = self.feature_extractor.lfr_config();
                    let all_lfr = if let Some((m, n)) = lfr_config {
                        apply_lfr(&self.raw_frame_buffer, m, n)
                    } else {
                        self.raw_frame_buffer.clone()
                    };

                    if all_lfr.len() > self.lfr_frames_emitted {
                        let mut new_lfr = all_lfr[self.lfr_frames_emitted..].to_vec();
                        self.lfr_frames_emitted = all_lfr.len();
                        if let Some(cmvn) = self.feature_extractor.cmvn() {
                            cmvn.apply(&mut new_lfr);
                        }
                        log::info!(
                            "Streaming ASR finalize with remaining buffer: samples={}, new_lfr_frames={}",
                            self.sample_buffer.len(),
                            new_lfr.len()
                        );
                        let _ = streaming.process_features(&new_lfr)?;
                    }
                }
                let final_tokens = streaming.finalize()?;
                let final_text = tokenizer.decode(&final_tokens, true);
                append_incremental_text(&mut self.partial_result, &final_text);
                let raw_text = self.partial_result.clone();
                log::info!(
                    "Streaming ASR final: tokens={}, raw='{}'",
                    final_tokens.len(),
                    raw_text
                );
                normalize_transcript_with_hotwords(&raw_text, &hotwords)
            }
            Some(AsrBackend::Whole(model)) => {
                if !self.sample_buffer.is_empty() {
                    self.whole_audio_buffer
                        .extend_from_slice(&self.sample_buffer);
                    self.sample_buffer.clear();
                }

                if self.whole_audio_buffer.is_empty() {
                    normalize_transcript_with_hotwords(&self.partial_result, &hotwords)
                } else {
                    let features = self.feature_extractor.extract(&self.whole_audio_buffer)?;
                    if features.is_empty() {
                        normalize_transcript_with_hotwords(&self.partial_result, &hotwords)
                    } else {
                        let final_tokens = model.infer(&features)?;
                        let final_text = tokenizer.decode(&final_tokens, true);
                        let best_effort = if final_text.is_empty() {
                            self.partial_result.clone()
                        } else {
                            final_text
                        };
                        log::info!("Whole-model ASR final: '{}'", best_effort);
                        normalize_transcript_with_hotwords(&best_effort, &hotwords)
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
        self.raw_frame_buffer.clear();
        self.lfr_frames_emitted = 0;
        self.is_first_chunk = true;

        if let Some(ref mut backend) = self.backend {
            backend.reset();
        }
    }

    pub fn reconfigure(&mut self, config: AsrConfig) {
        self.config = config;
    }

    #[allow(dead_code)]
    fn render_transcript(&self, text: &str, is_final: bool) -> String {
        let _ = is_final;
        let _ = &self.config;
        normalize_transcript_with_hotwords(text, &self.config.hotwords)
    }
}

#[derive(Debug, Clone)]
struct ModelRuntimeConfig {
    feature_config: FBankConfig,
    streaming_chunk_size: usize,
    encoder_left_context: usize,
    samples_per_window: usize,
    samples_per_step: usize,
    predictor_tail_threshold: f32,
}

impl Default for ModelRuntimeConfig {
    fn default() -> Self {
        let feature_config = FBankConfig::default();
        Self {
            samples_per_window: 8_000,
            samples_per_step: 8_000,
            streaming_chunk_size: 67,
            encoder_left_context: 0,
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
                .and_then(|encoder| encoder.streaming_chunk_size())
            {
                config.streaming_chunk_size = chunk_size;
            }
            if let Some(left_ctx) = model_yaml
                .encoder_conf
                .as_ref()
                .and_then(|encoder| encoder.encoder_left_context())
            {
                config.encoder_left_context = left_ctx;
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
            config.samples_per_window =
                streaming_samples_for_chunk(&config.feature_config, config.streaming_chunk_size);
            config.samples_per_step = streaming_step_samples_for_chunk(
                &config.feature_config,
                config.streaming_chunk_size,
            );
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
    #[serde(default)]
    stride: Vec<usize>,
}

#[derive(Debug, Deserialize)]
struct ModelYamlPredictorConf {
    #[serde(default)]
    tail_threshold: Option<f32>,
}

impl ModelYamlEncoderConf {
    fn streaming_chunk_size(&self) -> Option<usize> {
        self.stride
            .iter()
            .copied()
            .max()
            .filter(|value| *value > 0)
            .or_else(|| self.chunk_size_value())
    }

    fn chunk_size_value(&self) -> Option<usize> {
        self.chunk_size
            .iter()
            .copied()
            .max()
            .filter(|value| *value > 0)
    }

    /// Encoder left context = chunk_size[last] - stride[last].
    /// E.g. chunk_size=[12,15], stride=[8,10] → 15-10=5.
    fn encoder_left_context(&self) -> Option<usize> {
        let cs = self.chunk_size.last().copied()?;
        let st = self.stride.last().copied()?;
        if cs > st && st > 0 {
            Some(cs - st)
        } else {
            None
        }
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

fn streaming_step_samples_for_chunk(feature_config: &FBankConfig, chunk_size: usize) -> usize {
    let frame_shift = feature_config.sample_rate * feature_config.frame_shift_ms / 1000;
    if let Some((_lfr_m, lfr_n)) = feature_config.lfr {
        (chunk_size.max(1) * lfr_n * frame_shift).max(frame_shift)
    } else {
        (chunk_size.max(1) * frame_shift).max(frame_shift)
    }
}

fn append_incremental_text(base: &mut String, addition: &str) {
    if addition.is_empty() {
        return;
    }
    if base.is_empty() {
        base.push_str(addition);
        return;
    }

    let max_overlap = base.len().min(addition.len());
    let overlap = (0..=max_overlap)
        .rev()
        .find(|len| {
            base.is_char_boundary(base.len() - len)
                && addition.is_char_boundary(*len)
                && base[base.len() - len..] == addition[..*len]
        })
        .unwrap_or(0);

    base.push_str(&addition[overlap..]);
}

impl Default for AsrEngine {
    fn default() -> Self {
        Self::new(AsrConfig::default()).expect("Failed to create default ASR engine")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotwords::Hotword;

    #[test]
    fn online_streaming_prefers_stride_for_chunk_size() {
        let encoder = ModelYamlEncoderConf {
            chunk_size: vec![12, 15],
            stride: vec![8, 10],
        };

        assert_eq!(encoder.streaming_chunk_size(), Some(10));
        assert_eq!(encoder.chunk_size_value(), Some(15));
    }

    #[test]
    fn online_streaming_uses_window_and_hop_samples() {
        let config = FBankConfig {
            sample_rate: 16_000,
            frame_length_ms: 25,
            frame_shift_ms: 10,
            num_mel_bins: 80,
            lfr: Some((7, 6)),
            ..FBankConfig::default()
        };

        assert_eq!(streaming_samples_for_chunk(&config, 10), 10_000);
        assert_eq!(streaming_step_samples_for_chunk(&config, 10), 9_600);
    }

    #[test]
    fn append_incremental_text_deduplicates_overlap() {
        let mut text = "中文流式语音".to_string();
        append_incremental_text(&mut text, "语音识别模型");
        append_incremental_text(&mut text, "模型，适合低延迟实时转写");

        assert_eq!(text, "中文流式语音识别模型，适合低延迟实时转写");
    }

    #[test]
    fn render_transcript_uses_configured_hotwords() {
        let engine = AsrEngine::new(AsrConfig {
            hotwords: vec![
                Hotword {
                    word: "Chroma".to_string(),
                    weight: 90,
                },
                Hotword {
                    word: "Pinecone".to_string(),
                    weight: 90,
                },
            ],
            ..AsrConfig::default()
        })
        .expect("engine");

        assert_eq!(
            engine.render_transcript("chroma pinecone", false),
            "Chroma Pinecone"
        );
    }
}

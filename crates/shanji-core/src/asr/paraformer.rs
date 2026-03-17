//! FunASR Paraformer ONNX inference
//!
//! Implements streaming and non-streaming inference for Paraformer models.
//! Paraformer uses CIF (Continuous Integrate-and-Fire) for alignment.

use crate::error::{AppError, Result};
use ndarray::{Array1, Array3, Axis};
use ort::session::Session;
use ort::value::{DynTensor, Tensor, TensorElementType};
use std::collections::VecDeque;

/// Paraformer encoder wrapper
pub struct ParaformerEncoder {
    session: Session,
    /// Streaming cache
    cache: Option<EncoderCache>,
}

/// Encoder cache for streaming
#[derive(Clone)]
pub struct EncoderCache {
    /// Previous hidden states
    pub hidden: Array3<f32>,
    /// Previous positions
    pub pos: Array1<i64>,
}

#[derive(Clone)]
struct OnlineCifState {
    residual_alpha: f32,
    residual_frame: Vec<f32>,
    tail_threshold: f32,
}

impl OnlineCifState {
    fn new(hidden_size: usize, tail_threshold: f32) -> Self {
        Self {
            residual_alpha: 0.0,
            residual_frame: vec![0.0; hidden_size],
            tail_threshold,
        }
    }

    fn reset(&mut self) {
        self.residual_alpha = 0.0;
        self.residual_frame.fill(0.0);
    }

    fn build_acoustic_embeds(
        &mut self,
        encoder_out: Option<&Array3<f32>>,
        alphas: &[f32],
        is_final: bool,
    ) -> Result<Option<(Array3<f32>, i64)>> {
        let hidden_size = self.residual_frame.len();
        if hidden_size == 0 {
            return Ok(None);
        }

        let mut integrate = self.residual_alpha.max(0.0);
        let mut frames = self
            .residual_frame
            .iter()
            .map(|value| *value * integrate)
            .collect::<Vec<_>>();
        let mut emitted = Vec::new();

        if let Some(encoder_out) = encoder_out {
            let shape = encoder_out.shape();
            if shape[0] == 0 || shape[2] != hidden_size {
                return Err(AppError::Asr(format!(
                    "Unexpected encoder output shape for CIF: {:?}",
                    shape
                )));
            }

            for (t, alpha) in alphas.iter().copied().enumerate() {
                let hidden = encoder_out.index_axis(Axis(1), t);
                if alpha + integrate < 1.0 {
                    integrate += alpha;
                    for i in 0..hidden_size {
                        frames[i] += alpha * hidden[[0, i]];
                    }
                } else {
                    let remain = 1.0 - integrate;
                    for i in 0..hidden_size {
                        frames[i] += remain * hidden[[0, i]];
                    }
                    emitted.extend_from_slice(&frames);

                    integrate = alpha + integrate - 1.0;
                    for i in 0..hidden_size {
                        frames[i] = integrate * hidden[[0, i]];
                    }
                }
            }
        }

        if is_final && integrate > 0.0 {
            let alpha = self.tail_threshold.max(0.0);
            if alpha + integrate >= 1.0 {
                emitted.extend_from_slice(&frames);
                integrate = alpha + integrate - 1.0;
                frames.fill(0.0);
            } else {
                integrate += alpha;
            }
        }

        if integrate > 0.0 {
            for (avg, sum) in self.residual_frame.iter_mut().zip(frames.iter()) {
                *avg = *sum / integrate;
            }
        } else {
            self.residual_frame.fill(0.0);
        }
        self.residual_alpha = integrate;

        if emitted.is_empty() {
            return Ok(None);
        }

        let token_count = emitted.len() / hidden_size;
        let acoustic_embeds = Array3::from_shape_vec((1, token_count, hidden_size), emitted)
            .map_err(|e| AppError::Asr(format!("Invalid acoustic embeds shape: {}", e)))?;
        Ok(Some((acoustic_embeds, token_count as i64)))
    }
}

#[derive(Clone)]
struct OnlineDecoderCache {
    input_name: String,
    output_name: String,
    channels: usize,
    width: usize,
    data: Vec<f32>,
}

impl OnlineDecoderCache {
    fn zeroed(input_name: String, output_name: String, channels: usize, width: usize) -> Self {
        Self {
            input_name,
            output_name,
            channels,
            width,
            data: vec![0.0; channels * width],
        }
    }

    fn from_decoder_session(session: &Session) -> Result<Vec<Self>> {
        let mut entries = session
            .inputs()
            .iter()
            .filter_map(|input| {
                let name = input.name().to_string();
                let suffix = name.strip_prefix("in_cache_")?.parse::<usize>().ok()?;
                let dims = match input.dtype() {
                    ort::value::ValueType::Tensor { shape, .. } => shape.clone(),
                    _ => return None,
                };
                let channels = dims.get(1).copied().unwrap_or(512).max(1) as usize;
                let width = dims.get(2).copied().unwrap_or(10).max(1) as usize;
                Some((
                    suffix,
                    Self::zeroed(name, format!("out_cache_{}", suffix), channels, width),
                ))
            })
            .collect::<Vec<_>>();

        entries.sort_by_key(|(suffix, _)| *suffix);
        if entries.is_empty() {
            return Err(AppError::Asr(
                "Streaming decoder cache inputs not found".to_string(),
            ));
        }

        Ok(entries.into_iter().map(|(_, entry)| entry).collect())
    }

    fn tensor(&self) -> Result<DynTensor> {
        Tensor::from_array((
            [1usize, self.channels, self.width],
            self.data.clone().into_boxed_slice(),
        ))
        .map(|tensor| tensor.upcast())
        .map_err(|e| AppError::Asr(format!("Failed to create decoder cache tensor: {}", e)))
    }

    fn update_from_outputs(&mut self, outputs: &ort::session::SessionOutputs<'_>) -> Result<()> {
        let (shape, data) = outputs[self.output_name.as_str()]
            .try_extract_tensor::<f32>()
            .map_err(|e| {
                AppError::Asr(format!(
                    "Failed to extract decoder cache {}: {}",
                    self.output_name, e
                ))
            })?;

        self.channels = shape.get(1).copied().unwrap_or(self.channels as i64) as usize;
        self.width = shape.get(2).copied().unwrap_or(self.width as i64) as usize;
        self.data = data.to_vec();
        Ok(())
    }
}

enum StreamingRuntime {
    Legacy {
        encoder: ParaformerEncoder,
        decoder: Option<ParaformerDecoder>,
    },
    Online {
        encoder: Session,
        decoder: Session,
        cif_state: OnlineCifState,
        decoder_caches: Vec<OnlineDecoderCache>,
        last_encoder_out: Option<Array3<f32>>,
        last_encoder_len: Vec<i64>,
    },
}

impl ParaformerEncoder {
    pub fn new(session: Session) -> Self {
        Self {
            session,
            cache: None,
        }
    }

    /// Run encoder inference
    ///
    /// Input:
    ///   - speech: [batch, frames, 80] - FBank features
    ///   - speech_lengths: [batch] - frame counts
    ///   - (streaming) cache: previous hidden states
    ///
    /// Output:
    ///   - encoder_out: [batch, frames, hidden_dim]
    ///   - encoder_out_lens: [batch]
    ///   - (streaming) cache: updated hidden states
    pub fn encode(
        &mut self,
        features: &[Vec<f32>],
        is_streaming: bool,
    ) -> Result<(Array3<f32>, Vec<i64>)> {
        let batch_size = 1;
        let num_frames = features.len();
        let num_mels = features.first().map(|frame| frame.len()).unwrap_or(80);

        if num_frames == 0 {
            return Ok((Array3::zeros((batch_size, 0, 256)), vec![0]));
        }

        // Prepare input tensor [batch, frames, feat_dim].
        // Streaming Paraformer encoders may expect either raw 80-dim FBanks or
        // 560-dim LFR features, so we must preserve the actual feature width.
        let mut speech_data = Vec::with_capacity(batch_size * num_frames * num_mels);
        for frame in features {
            for &val in frame.iter().take(num_mels) {
                speech_data.push(val);
            }
            // Pad if frame is shorter than the expected feature width.
            for _ in frame.len()..num_mels {
                speech_data.push(0.0);
            }
        }

        // Build inputs based on streaming mode
        let outputs = if is_streaming && self.cache.is_some() {
            // Streaming mode with cache
            let cache = self.cache.as_ref().unwrap();

            let speech_tensor = Tensor::from_array((
                [batch_size, num_frames, num_mels],
                speech_data.into_boxed_slice(),
            ))
            .map_err(|e| AppError::Asr(format!("Failed to create speech tensor: {}", e)))?;
            let lengths_tensor = create_length_tensor_for_input(
                &self.session,
                "speech_lengths",
                &[num_frames as i64],
            )?;
            let cache_tensor = Tensor::from_array((
                [1usize, cache.hidden.shape()[1], cache.hidden.shape()[2]],
                cache.hidden.clone().into_raw_vec().into_boxed_slice(),
            ))
            .map_err(|e| AppError::Asr(format!("Failed to create cache tensor: {}", e)))?;
            let cache_lengths_tensor =
                create_length_tensor_for_input(&self.session, "cache_lengths", &[cache.pos[0]])
                    .map_err(|e| {
                        AppError::Asr(format!("Failed to create cache lengths tensor: {}", e))
                    })?;

            self.session
                .run(ort::inputs! {
                    "speech" => speech_tensor,
                    "speech_lengths" => lengths_tensor,
                    "cache" => cache_tensor,
                    "cache_lengths" => cache_lengths_tensor,
                })
                .map_err(|e| AppError::Asr(format!("Encoder inference failed: {}", e)))?
        } else {
            // Non-streaming or first chunk
            let speech_tensor = Tensor::from_array((
                [batch_size, num_frames, num_mels],
                speech_data.into_boxed_slice(),
            ))
            .map_err(|e| AppError::Asr(format!("Failed to create speech tensor: {}", e)))?;
            let lengths_tensor = create_length_tensor_for_input(
                &self.session,
                "speech_lengths",
                &[num_frames as i64],
            )?;

            self.session
                .run(ort::inputs! {
                    "speech" => speech_tensor,
                    "speech_lengths" => lengths_tensor,
                })
                .map_err(|e| AppError::Asr(format!("Encoder inference failed: {}", e)))?
        };

        // Extract outputs - ort 2.0 returns (shape, data) tuple
        let (encoder_out_shape, encoder_out_data) = outputs["encoder_out"]
            .try_extract_tensor::<f32>()
            .map_err(|e| AppError::Asr(format!("Failed to extract encoder output: {}", e)))?;

        let encoder_out = Array3::from_shape_vec(
            (
                encoder_out_shape[0] as usize,
                encoder_out_shape[1] as usize,
                encoder_out_shape[2] as usize,
            ),
            encoder_out_data.to_vec(),
        )
        .map_err(|e| AppError::Asr(format!("Invalid encoder output shape: {}", e)))?;

        let encoder_out_lens = extract_i64_tensor(&outputs["encoder_out_lens"])
            .map_err(|e| AppError::Asr(format!("Failed to extract encoder lengths: {}", e)))?;

        // Update cache if streaming
        if is_streaming {
            if let Ok((cache_shape, cache_data)) = outputs["cache_out"].try_extract_tensor::<f32>()
            {
                let hidden = Array3::from_shape_vec(
                    (
                        cache_shape[0] as usize,
                        cache_shape[1] as usize,
                        cache_shape[2] as usize,
                    ),
                    cache_data.to_vec(),
                )
                .unwrap_or_else(|_| Array3::zeros((1, 0, 256)));

                let pos = extract_i64_tensor(&outputs["cache_lengths_out"])
                    .ok()
                    .and_then(|data| data.first().copied())
                    .map(|p| ndarray::array![p])
                    .unwrap_or_else(|| ndarray::array![0i64]);

                self.cache = Some(EncoderCache { hidden, pos });
            }
        }

        Ok((encoder_out, encoder_out_lens))
    }

    /// Reset cache for new utterance
    pub fn reset(&mut self) {
        self.cache = None;
    }
}

/// Paraformer decoder wrapper (CTC branch)
pub struct ParaformerDecoder {
    session: Session,
}

impl ParaformerDecoder {
    pub fn new(session: Session) -> Self {
        Self { session }
    }

    /// Decode encoder output to token probabilities
    ///
    /// Input:
    ///   - encoder_out: [batch, frames, hidden_dim]
    ///   - encoder_out_lens: [batch]
    ///
    /// Output:
    ///   - ctc_logits: [batch, frames, vocab_size]
    pub fn decode(
        &mut self,
        encoder_out: &Array3<f32>,
        encoder_out_lens: &[i64],
    ) -> Result<Array3<f32>> {
        let shape = encoder_out.shape();
        let encoder_data = encoder_out.clone().into_raw_vec();

        let encoder_tensor = Tensor::from_array((
            [shape[0], shape[1], shape[2]],
            encoder_data.into_boxed_slice(),
        ))
        .map_err(|e| AppError::Asr(format!("Failed to create encoder tensor: {}", e)))?;
        let lengths_tensor =
            create_length_tensor_for_input(&self.session, "encoder_out_lens", encoder_out_lens)
                .map_err(|e| AppError::Asr(format!("Failed to create lengths tensor: {}", e)))?;

        let outputs = self
            .session
            .run(ort::inputs! {
                "encoder_out" => encoder_tensor,
                "encoder_out_lens" => lengths_tensor,
            })
            .map_err(|e| AppError::Asr(format!("Decoder inference failed: {}", e)))?;

        let (ctc_shape, ctc_data) = outputs["ctc_logits"]
            .try_extract_tensor::<f32>()
            .map_err(|e| AppError::Asr(format!("Failed to extract CTC logits: {}", e)))?;

        let ctc_logits = Array3::from_shape_vec(
            (
                ctc_shape[0] as usize,
                ctc_shape[1] as usize,
                ctc_shape[2] as usize,
            ),
            ctc_data.to_vec(),
        )
        .map_err(|e| AppError::Asr(format!("Invalid CTC logits shape: {}", e)))?;

        Ok(ctc_logits)
    }

    /// Greedy CTC decode
    pub fn greedy_decode(&self, ctc_logits: &Array3<f32>, blank_id: i32) -> Vec<i32> {
        greedy_decode_logits(ctc_logits, blank_id)
    }
}

pub struct WholeModelParaformer {
    session: Session,
}

impl WholeModelParaformer {
    pub fn new(session: Session) -> Self {
        Self { session }
    }

    /// Returns the feature dimension the model expects for its "speech" input.
    /// 560 means LFR(7,6) is required; 80 means raw FBank; defaults to 560.
    pub fn expected_feat_dim(&self) -> usize {
        use ort::value::ValueType;
        self.session
            .inputs()
            .iter()
            .find(|i| i.name() == "speech")
            .and_then(|i| match i.dtype() {
                ValueType::Tensor { shape, .. } => shape.as_ref().last().map(|&d| d as usize),
                _ => None,
            })
            .filter(|&d| d > 0)
            .unwrap_or(560)
    }

    pub fn infer(&mut self, features: &[Vec<f32>]) -> Result<Vec<i32>> {
        let batch_size = 1usize;
        let num_frames = features.len();
        // Get actual feature dimension from first frame (could be 80 or 560 after LFR)
        let num_mels = if features.is_empty() {
            80
        } else {
            features[0].len()
        };

        if num_frames == 0 {
            return Ok(Vec::new());
        }

        let mut speech_data = Vec::with_capacity(batch_size * num_frames * num_mels);
        for frame in features {
            for &val in frame.iter().take(num_mels) {
                speech_data.push(val);
            }
            // Pad if frame is shorter than expected
            for _ in frame.len()..num_mels {
                speech_data.push(0.0);
            }
        }

        let speech_tensor = Tensor::from_array((
            [batch_size, num_frames, num_mels],
            speech_data.into_boxed_slice(),
        ))
        .map_err(|e| AppError::Asr(format!("Failed to create speech tensor: {}", e)))?;
        let lengths_tensor =
            create_length_tensor_for_input(&self.session, "speech_lengths", &[num_frames as i64])
                .map_err(|e| AppError::Asr(format!("Failed to create lengths tensor: {}", e)))?;

        let outputs = self
            .session
            .run(ort::inputs! {
                "speech" => speech_tensor,
                "speech_lengths" => lengths_tensor,
            })
            .map_err(|e| AppError::Asr(format!("Model inference failed: {}", e)))?;

        // Try "am_scores" first (FunASR standard export), then fall back to "logits".
        // SessionOutputs indexing panics on a missing name, so probe safely first.
        let (logits_shape, logits_data) = if let Some(value) = outputs.get("am_scores") {
            value
                .try_extract_tensor::<f32>()
                .map_err(|e| AppError::Asr(format!("Failed to extract am_scores: {}", e)))?
        } else if let Some(value) = outputs.get("logits") {
            value
                .try_extract_tensor::<f32>()
                .map_err(|e| AppError::Asr(format!("Failed to extract logits: {}", e)))?
        } else {
            let available_outputs = outputs.keys().collect::<Vec<_>>().join(", ");
            return Err(AppError::Asr(format!(
                "Model output missing: neither am_scores nor logits found (available: [{}])",
                available_outputs
            )));
        };

        let logits = Array3::from_shape_vec(
            (
                logits_shape[0] as usize,
                logits_shape[1] as usize,
                logits_shape[2] as usize,
            ),
            logits_data.to_vec(),
        )
        .map_err(|e| AppError::Asr(format!("Invalid logits shape: {}", e)))?;

        Ok(greedy_decode_logits(&logits, 0))
    }
}

fn create_length_tensor_for_input(
    session: &Session,
    input_name: &str,
    values: &[i64],
) -> Result<DynTensor> {
    let tensor_type = session
        .inputs()
        .iter()
        .find(|input| input.name() == input_name)
        .and_then(|input| input.dtype().tensor_type());

    match tensor_type {
        Some(TensorElementType::Int32) => Tensor::from_array((
            [values.len()],
            values
                .iter()
                .map(|value| *value as i32)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        ))
        .map(|tensor| tensor.upcast())
        .map_err(|e| AppError::Asr(format!("Failed to create int32 tensor: {}", e))),
        _ => Tensor::from_array(([values.len()], values.to_vec().into_boxed_slice()))
            .map(|tensor| tensor.upcast())
            .map_err(|e| AppError::Asr(format!("Failed to create int64 tensor: {}", e))),
    }
}

fn extract_i64_tensor(value: &ort::value::DynValue) -> Result<Vec<i64>> {
    if let Ok((_, data)) = value.try_extract_tensor::<i64>() {
        return Ok(data.to_vec());
    }
    if let Ok((_, data)) = value.try_extract_tensor::<i32>() {
        return Ok(data.iter().map(|value| *value as i64).collect());
    }

    Err(AppError::Asr(
        "Unsupported integer tensor type; expected int32 or int64".to_string(),
    ))
}

fn uses_online_streaming_interface(encoder: &Session, decoder: Option<&Session>) -> bool {
    let encoder_outputs = encoder
        .outputs()
        .iter()
        .map(|output| output.name())
        .collect::<Vec<_>>();
    let decoder_inputs = decoder
        .map(|session| {
            session
                .inputs()
                .iter()
                .map(|input| input.name())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    encoder_outputs.contains(&"enc")
        && encoder_outputs.contains(&"alphas")
        && decoder_inputs.contains(&"acoustic_embeds")
}

fn run_online_encoder(
    session: &mut Session,
    features: &[Vec<f32>],
) -> Result<(Array3<f32>, Vec<i64>, Vec<f32>)> {
    let batch_size = 1usize;
    let num_frames = features.len();
    let feat_dim = features.first().map(|frame| frame.len()).unwrap_or(0);
    if num_frames == 0 || feat_dim == 0 {
        return Err(AppError::Asr(
            "Streaming online encoder received empty features".to_string(),
        ));
    }

    let mut speech_data = Vec::with_capacity(batch_size * num_frames * feat_dim);
    for frame in features {
        speech_data.extend_from_slice(frame);
    }

    let speech_tensor = Tensor::from_array((
        [batch_size, num_frames, feat_dim],
        speech_data.into_boxed_slice(),
    ))
    .map_err(|e| AppError::Asr(format!("Failed to create online speech tensor: {}", e)))?;
    let lengths_tensor =
        create_length_tensor_for_input(session, "speech_lengths", &[num_frames as i64])?;

    let outputs = session
        .run(ort::inputs! {
            "speech" => speech_tensor,
            "speech_lengths" => lengths_tensor,
        })
        .map_err(|e| AppError::Asr(format!("Online encoder inference failed: {}", e)))?;

    let encoder_out = extract_array3_f32(&outputs["enc"], "enc")?;
    let encoder_out_lens = extract_i64_tensor(&outputs["enc_len"])
        .map_err(|e| AppError::Asr(format!("Failed to extract enc_len: {}", e)))?;
    let (_, alphas_data) = outputs["alphas"]
        .try_extract_tensor::<f32>()
        .map_err(|e| AppError::Asr(format!("Failed to extract alphas: {}", e)))?;

    Ok((encoder_out, encoder_out_lens, alphas_data.to_vec()))
}

fn run_online_decoder(
    session: &mut Session,
    encoder_out: &Array3<f32>,
    encoder_out_lens: &[i64],
    acoustic_embeds: &Array3<f32>,
    acoustic_embeds_len: i64,
    decoder_caches: &mut [OnlineDecoderCache],
) -> Result<Vec<i32>> {
    let encoder_shape = encoder_out.shape();
    let acoustic_shape = acoustic_embeds.shape();

    let encoder_tensor = Tensor::from_array((
        [encoder_shape[0], encoder_shape[1], encoder_shape[2]],
        encoder_out.clone().into_raw_vec().into_boxed_slice(),
    ))
    .map_err(|e| AppError::Asr(format!("Failed to create online enc tensor: {}", e)))?;
    let encoder_lengths_tensor =
        create_length_tensor_for_input(session, "enc_len", encoder_out_lens)?;
    let acoustic_tensor = Tensor::from_array((
        [acoustic_shape[0], acoustic_shape[1], acoustic_shape[2]],
        acoustic_embeds.clone().into_raw_vec().into_boxed_slice(),
    ))
    .map_err(|e| AppError::Asr(format!("Failed to create acoustic embeds tensor: {}", e)))?;
    let acoustic_lengths_tensor =
        create_length_tensor_for_input(session, "acoustic_embeds_len", &[acoustic_embeds_len])?;

    let mut inputs = vec![
        ("enc".to_string(), encoder_tensor.upcast()),
        ("enc_len".to_string(), encoder_lengths_tensor),
        ("acoustic_embeds".to_string(), acoustic_tensor.upcast()),
        ("acoustic_embeds_len".to_string(), acoustic_lengths_tensor),
    ];
    for cache in decoder_caches.iter() {
        inputs.push((cache.input_name.clone(), cache.tensor()?));
    }

    let outputs = session
        .run(inputs)
        .map_err(|e| AppError::Asr(format!("Online decoder inference failed: {}", e)))?;

    for cache in decoder_caches.iter_mut() {
        cache.update_from_outputs(&outputs)?;
    }

    let mut sample_ids = extract_i64_tensor(&outputs["sample_ids"])
        .map_err(|e| AppError::Asr(format!("Failed to extract sample_ids: {}", e)))?;
    sample_ids.truncate(acoustic_embeds_len.max(0) as usize);

    Ok(sample_ids.into_iter().map(|id| id as i32).collect())
}

fn extract_array3_f32(value: &ort::value::DynValue, output_name: &str) -> Result<Array3<f32>> {
    let (shape, data) = value
        .try_extract_tensor::<f32>()
        .map_err(|e| AppError::Asr(format!("Failed to extract {} tensor: {}", output_name, e)))?;
    if shape.len() != 3 {
        return Err(AppError::Asr(format!(
            "Unexpected {} tensor rank {}; expected 3",
            output_name,
            shape.len()
        )));
    }

    Array3::from_shape_vec(
        (shape[0] as usize, shape[1] as usize, shape[2] as usize),
        data.to_vec(),
    )
    .map_err(|e| AppError::Asr(format!("Invalid {} tensor shape: {}", output_name, e)))
}

fn greedy_decode_logits(ctc_logits: &Array3<f32>, blank_id: i32) -> Vec<i32> {
    let mut result = Vec::new();
    let mut prev_id = -1i32;

    for frame in ctc_logits.axis_iter(Axis(1)) {
        let (max_idx, _) = frame
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap_or((0, &0.0));

        let id = max_idx as i32;
        if id != blank_id && id != prev_id {
            result.push(id);
            prev_id = id;
        }
    }

    result
}

impl WholeModelParaformer {
    pub fn reset(&mut self) {}
}

impl ParaformerDecoder {
    pub fn greedy_decode_legacy(&self, ctc_logits: &Array3<f32>, blank_id: i32) -> Vec<i32> {
        let mut result = Vec::new();
        let mut prev_id = -1i32;

        // Get argmax for each frame
        for frame in ctc_logits.axis_iter(Axis(1)) {
            let (max_idx, _) = frame
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                .unwrap_or((0, &0.0));

            let id = max_idx as i32;
            if id != blank_id && id != prev_id {
                result.push(id);
                prev_id = id;
            }
        }

        result
    }
}

/// Streaming Paraformer ASR
///
/// Implements the FunASR streaming protocol:
/// - Each encoder call receives `left_context + stride + right_context` frames
/// - Alphas for look-back (first `left_context`) and look-ahead (last `right_context`)
///   are zeroed before CIF processing
/// - Full encoder output (all frames) is passed to the decoder
/// - Feature cache of `left_context + right_context` frames is maintained between chunks
pub struct StreamingParaformer {
    runtime: StreamingRuntime,
    /// Feature buffer for accumulating LFR frames until we have `stride` frames
    feature_buffer: VecDeque<Vec<f32>>,
    /// Stride: number of new LFR frames per chunk (e.g. 10)
    chunk_size: usize,
    /// Left context frames (look-back, e.g. 5)
    left_context: usize,
    /// Right context frames (look-ahead, e.g. 5)
    right_context: usize,
    /// Cached feature frames from previous chunk: left_context + right_context frames.
    /// Initialized to zeros for the first chunk.
    feature_cache: Vec<Vec<f32>>,
    /// Feature dimension (e.g. 560 for LFR features)
    feat_dim: usize,
    /// Whether the cache has been initialized
    cache_initialized: bool,
    /// Counter for diagnostic logging
    chunk_index: usize,
}

impl StreamingParaformer {
    pub fn new(
        encoder: Session,
        decoder: Option<Session>,
        chunk_size: usize,
        encoder_left_context: usize,
        tail_threshold: f32,
    ) -> Result<Self> {
        // Right context equals left context for standard paraformer streaming
        let right_context = encoder_left_context;
        let runtime = if uses_online_streaming_interface(&encoder, decoder.as_ref()) {
            // Dump all encoder inputs/outputs for diagnostics
            let enc_inputs: Vec<String> = encoder
                .inputs()
                .iter()
                .map(|i| format!("{}:{:?}", i.name(), i.dtype()))
                .collect();
            let enc_outputs: Vec<String> = encoder
                .outputs()
                .iter()
                .map(|o| format!("{}:{:?}", o.name(), o.dtype()))
                .collect();
            log::info!("[DIAG] Online encoder inputs: {:?}", enc_inputs);
            log::info!("[DIAG] Online encoder outputs: {:?}", enc_outputs);

            let decoder = decoder.ok_or_else(|| {
                AppError::Asr(
                    "Official online streaming Paraformer requires a decoder model".to_string(),
                )
            })?;
            let dec_inputs: Vec<String> = decoder
                .inputs()
                .iter()
                .map(|i| format!("{}:{:?}", i.name(), i.dtype()))
                .collect();
            let dec_outputs: Vec<String> = decoder
                .outputs()
                .iter()
                .map(|o| format!("{}:{:?}", o.name(), o.dtype()))
                .collect();
            log::info!("[DIAG] Online decoder inputs: {:?}", dec_inputs);
            log::info!("[DIAG] Online decoder outputs: {:?}", dec_outputs);
            let decoder_caches = OnlineDecoderCache::from_decoder_session(&decoder)?;
            let hidden_size = decoder
                .inputs()
                .iter()
                .find(|input| input.name() == "acoustic_embeds")
                .and_then(|input| match input.dtype() {
                    ort::value::ValueType::Tensor { shape, .. } => {
                        shape.as_ref().get(2).copied().map(|value| value as usize)
                    }
                    _ => None,
                })
                .unwrap_or(512);

            StreamingRuntime::Online {
                encoder,
                decoder,
                cif_state: OnlineCifState::new(hidden_size, tail_threshold),
                decoder_caches,
                last_encoder_out: None,
                last_encoder_len: vec![0],
            }
        } else {
            StreamingRuntime::Legacy {
                encoder: ParaformerEncoder::new(encoder),
                decoder: decoder.map(ParaformerDecoder::new),
            }
        };

        log::info!(
            "StreamingParaformer: stride={}, left_context={}, right_context={}, total_encoder_input={}",
            chunk_size, encoder_left_context, right_context,
            encoder_left_context + chunk_size + right_context
        );

        Ok(Self {
            runtime,
            feature_buffer: VecDeque::new(),
            chunk_size,
            left_context: encoder_left_context,
            right_context,
            feature_cache: Vec::new(),
            feat_dim: 0,
            cache_initialized: false,
            chunk_index: 0,
        })
    }

    /// Process a chunk of features
    ///
    /// Returns partial result if available
    pub fn process_features(&mut self, features: &[Vec<f32>]) -> Result<Option<Vec<i32>>> {
        // Add to buffer
        for frame in features {
            self.feature_buffer.push_back(frame.clone());
        }

        // Process if we have enough frames
        if self.feature_buffer.len() >= self.chunk_size {
            let chunk: Vec<Vec<f32>> = self.feature_buffer.drain(..self.chunk_size).collect();
            return self.process_chunk_internal(&chunk, false);
        }

        Ok(None)
    }

    /// Finalize and return remaining tokens
    pub fn finalize(&mut self) -> Result<Vec<i32>> {
        // Process remaining frames
        let remaining: Vec<Vec<f32>> = self.feature_buffer.drain(..).collect();

        Ok(self
            .process_chunk_internal(&remaining, true)?
            .unwrap_or_default())
    }

    /// Reset for new utterance
    pub fn reset(&mut self) {
        match &mut self.runtime {
            StreamingRuntime::Legacy { encoder, .. } => encoder.reset(),
            StreamingRuntime::Online {
                cif_state,
                decoder_caches,
                last_encoder_out,
                last_encoder_len,
                ..
            } => {
                cif_state.reset();
                for cache in decoder_caches {
                    cache.data.fill(0.0);
                }
                *last_encoder_out = None;
                last_encoder_len.clear();
                last_encoder_len.push(0);
            }
        }
        self.feature_buffer.clear();
        self.feature_cache.clear();
        self.cache_initialized = false;
        self.chunk_index = 0;
    }

    fn process_chunk_internal(
        &mut self,
        chunk: &[Vec<f32>],
        is_final: bool,
    ) -> Result<Option<Vec<i32>>> {
        match &mut self.runtime {
            StreamingRuntime::Legacy { encoder, decoder } => {
                if chunk.is_empty() {
                    return Ok(None);
                }

                let (encoder_out, _lens) = encoder.encode(chunk, true)?;
                if let Some(decoder) = decoder {
                    let ctc_logits =
                        decoder.decode(&encoder_out, &[encoder_out.shape()[1] as i64])?;
                    let tokens = decoder.greedy_decode(&ctc_logits, 0);
                    return Ok(Some(tokens));
                }
                Ok(None)
            }
            StreamingRuntime::Online {
                encoder,
                decoder,
                cif_state,
                decoder_caches,
                last_encoder_out,
                last_encoder_len,
            } => {
                let left_ctx = self.left_context;
                let right_ctx = self.right_context;
                let cache_size = left_ctx + right_ctx;
                let chunk_idx = self.chunk_index;
                self.chunk_index += 1;

                let (encoder_out, encoder_out_lens, alphas) = if chunk.is_empty() {
                    (None, last_encoder_len.clone(), Vec::new())
                } else {
                    // Initialize feature dimension and zero-cache on first chunk
                    if !self.cache_initialized && cache_size > 0 {
                        let dim = chunk.first().map(|f| f.len()).unwrap_or(560);
                        self.feat_dim = dim;
                        // Initialize cache with zero frames (no prior context)
                        self.feature_cache = vec![vec![0.0f32; dim]; cache_size];
                        self.cache_initialized = true;
                    }

                    // Build encoder input: [cache] + [new stride frames]
                    // Total = left_context + right_context + stride = cache_size + stride
                    let encoder_input = if cache_size > 0 && self.cache_initialized {
                        let mut combined = self.feature_cache.clone();
                        combined.extend_from_slice(chunk);
                        combined
                    } else {
                        chunk.to_vec()
                    };

                    // Update cache: last `cache_size` frames from the combined input
                    if cache_size > 0 {
                        let total = encoder_input.len();
                        let start = total.saturating_sub(cache_size);
                        self.feature_cache = encoder_input[start..].to_vec();
                    }

                    let (enc, enc_len, mut all_alphas) =
                        run_online_encoder(encoder, &encoder_input)?;

                    let enc_frames = enc.shape()[1];
                    let alpha_len = all_alphas.len();

                    // Zero out look-back alphas (first left_context positions)
                    for i in 0..left_ctx.min(alpha_len) {
                        all_alphas[i] = 0.0;
                    }
                    // Zero out look-ahead alphas (last right_context positions)
                    let suf_start = (left_ctx + self.chunk_size).min(alpha_len);
                    for i in suf_start..alpha_len {
                        all_alphas[i] = 0.0;
                    }

                    log::info!(
                        "[DIAG] chunk#{}: input_frames={} (cache={} + new={}), enc_out={}, alphas_len={}, zeroed=[0..{}]+[{}..{}]",
                        chunk_idx, encoder_input.len(), cache_size, chunk.len(),
                        enc_frames, alpha_len, left_ctx.min(alpha_len), suf_start, alpha_len
                    );

                    // Pass FULL encoder output (not trimmed) to decoder
                    *last_encoder_out = Some(enc.clone());
                    *last_encoder_len = enc_len.clone();
                    (Some(enc), enc_len, all_alphas)
                };

                let acoustic_embeds =
                    cif_state.build_acoustic_embeds(encoder_out.as_ref(), &alphas, is_final)?;

                let Some((acoustic_embeds, acoustic_embeds_len)) = acoustic_embeds else {
                    return Ok(None);
                };

                let memory = encoder_out
                    .as_ref()
                    .or(last_encoder_out.as_ref())
                    .ok_or_else(|| {
                        AppError::Asr(
                            "Streaming online decoder requires encoder memory for decoding"
                                .to_string(),
                        )
                    })?;

                let tokens = run_online_decoder(
                    decoder,
                    memory,
                    &encoder_out_lens,
                    &acoustic_embeds,
                    acoustic_embeds_len,
                    decoder_caches,
                )?;

                if tokens.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(tokens))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ctc_greedy_decode() {
        // Create dummy CTC logits: [1 batch, 4 frames, 5 vocab]
        let logits = Array3::from_shape_vec(
            (1, 4, 5),
            vec![
                0.1, 0.8, 0.05, 0.03, 0.02, // frame 0 -> token 1
                0.1, 0.8, 0.05, 0.03, 0.02, // frame 1 -> token 1 (repeat, should collapse)
                0.7, 0.1, 0.1, 0.05, 0.05, // frame 2 -> token 0 (blank)
                0.05, 0.05, 0.8, 0.05, 0.05, // frame 3 -> token 2
            ],
        )
        .unwrap();

        let tokens = greedy_decode_logits(&logits, 0);
        assert_eq!(tokens, vec![1, 2]);
    }
}

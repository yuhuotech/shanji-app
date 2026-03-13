//! FunASR Paraformer ONNX inference
//!
//! Implements streaming and non-streaming inference for Paraformer models.
//! Paraformer uses CIF (Continuous Integrate-and-Fire) for alignment.

use crate::error::{AppError, Result};
use ndarray::{Array1, Array3, Axis};
use ort::session::Session;
use ort::value::Tensor;
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
        let num_mels = 80;

        if num_frames == 0 {
            return Ok((Array3::zeros((batch_size, 0, 256)), vec![0]));
        }

        // Prepare input tensor [batch, frames, 80]
        let mut speech_data = Vec::with_capacity(batch_size * num_frames * num_mels);
        for frame in features {
            for &val in frame.iter().take(num_mels) {
                speech_data.push(val);
            }
            // Pad if frame is shorter than 80
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
            let lengths_tensor =
                Tensor::from_array(([batch_size], vec![num_frames as i64].into_boxed_slice()))
                    .map_err(|e| {
                        AppError::Asr(format!("Failed to create lengths tensor: {}", e))
                    })?;
            let cache_tensor = Tensor::from_array((
                [1usize, cache.hidden.shape()[1], cache.hidden.shape()[2]],
                cache.hidden.clone().into_raw_vec().into_boxed_slice(),
            ))
            .map_err(|e| AppError::Asr(format!("Failed to create cache tensor: {}", e)))?;
            let cache_lengths_tensor =
                Tensor::from_array(([1usize], vec![cache.pos[0]].into_boxed_slice())).map_err(
                    |e| AppError::Asr(format!("Failed to create cache lengths tensor: {}", e)),
                )?;

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
            let lengths_tensor =
                Tensor::from_array(([batch_size], vec![num_frames as i64].into_boxed_slice()))
                    .map_err(|e| {
                        AppError::Asr(format!("Failed to create lengths tensor: {}", e))
                    })?;

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

        let (_, encoder_out_lens_data) = outputs["encoder_out_lens"]
            .try_extract_tensor::<i64>()
            .map_err(|e| AppError::Asr(format!("Failed to extract encoder lengths: {}", e)))?;

        let encoder_out_lens: Vec<i64> = encoder_out_lens_data.to_vec();

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

                let pos = outputs["cache_lengths_out"]
                    .try_extract_tensor::<i64>()
                    .ok()
                    .and_then(|(_, data)| data.first().copied())
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
            Tensor::from_array(([shape[0]], encoder_out_lens.to_vec().into_boxed_slice()))
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

    pub fn infer(&mut self, features: &[Vec<f32>]) -> Result<Vec<i32>> {
        let batch_size = 1usize;
        let num_frames = features.len();
        // Get actual feature dimension from first frame (could be 80 or 560 after LFR)
        let num_mels = if features.is_empty() { 80 } else { features[0].len() };

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
        let lengths_tensor = Tensor::from_array((
            [batch_size],
            vec![num_frames as i32].into_boxed_slice(),
        ))
        .map_err(|e| AppError::Asr(format!("Failed to create lengths tensor: {}", e)))?;

        let outputs = self
            .session
            .run(ort::inputs! {
                "speech" => speech_tensor,
                "speech_lengths" => lengths_tensor,
            })
            .map_err(|e| AppError::Asr(format!("Model inference failed: {}", e)))?;

        let (logits_shape, logits_data) = outputs["logits"]
            .try_extract_tensor::<f32>()
            .map_err(|e| AppError::Asr(format!("Failed to extract logits: {}", e)))?;

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
pub struct StreamingParaformer {
    encoder: ParaformerEncoder,
    decoder: Option<ParaformerDecoder>,
    /// Feature buffer for accumulating frames
    feature_buffer: VecDeque<Vec<f32>>,
    /// Chunk size in frames (default 67 frames ~ 1 second)
    chunk_size: usize,
}

impl StreamingParaformer {
    pub fn new(encoder: Session, decoder: Option<Session>, chunk_size: usize) -> Self {
        Self {
            encoder: ParaformerEncoder::new(encoder),
            decoder: decoder.map(ParaformerDecoder::new),
            feature_buffer: VecDeque::new(),
            chunk_size,
        }
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

            // Encode
            let (encoder_out, _lens) = self.encoder.encode(&chunk, true)?;

            // Decode if decoder available
            if let Some(ref mut decoder) = self.decoder {
                let ctc_logits = decoder.decode(&encoder_out, &[encoder_out.shape()[1] as i64])?;
                let tokens = decoder.greedy_decode(&ctc_logits, 0);
                return Ok(Some(tokens));
            }
        }

        Ok(None)
    }

    /// Finalize and return remaining tokens
    pub fn finalize(&mut self) -> Result<Vec<i32>> {
        // Process remaining frames
        let remaining: Vec<Vec<f32>> = self.feature_buffer.drain(..).collect();

        if remaining.is_empty() {
            return Ok(Vec::new());
        }

        let (encoder_out, _lens) = self.encoder.encode(&remaining, true)?;

        if let Some(ref mut decoder) = self.decoder {
            let ctc_logits = decoder.decode(&encoder_out, &[encoder_out.shape()[1] as i64])?;
            let tokens = decoder.greedy_decode(&ctc_logits, 0);
            Ok(tokens)
        } else {
            Ok(Vec::new())
        }
    }

    /// Reset for new utterance
    pub fn reset(&mut self) {
        self.encoder.reset();
        self.feature_buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ctc_greedy_decode() {
        // Create dummy CTC logits: [1 batch, 10 frames, 5 vocab]
        let logits = Array3::from_shape_vec(
            (1, 10, 5),
            vec![
                0.1, 0.8, 0.05, 0.03, 0.02, // frame 0 -> token 1
                0.1, 0.8, 0.05, 0.03, 0.02, // frame 1 -> token 1 (repeat, should collapse)
                0.7, 0.1, 0.1, 0.05, 0.05, // frame 2 -> token 0 (blank)
                0.05, 0.05, 0.8, 0.05,
                0.05, // frame 3 -> token 2
                      // ... more frames
            ],
        )
        .unwrap();

        let decoder = ParaformerDecoder::new(
            // Mock session - would need actual ONNX in real test
            panic!("Mock session not implemented in test"),
        );

        let tokens = decoder.greedy_decode(&logits, 0);
        assert_eq!(tokens, vec![1, 2]);
    }
}

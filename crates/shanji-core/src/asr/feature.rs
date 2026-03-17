//! Audio feature extraction for ASR
//!
//! Extracts FBank (Filter Bank) features from raw audio.
//! This is the standard input for Paraformer models.

use crate::error::{AppError, Result};
use realfft::RealFftPlanner;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// CMVN (Cepstral Mean and Variance Normalization) statistics
/// loaded from FunASR am.mvn file (Kaldi Nnet format).
#[derive(Debug, Clone)]
pub struct CmvnStats {
    /// Negative means from <AddShift> section — added to features
    pub shift: Vec<f32>,
    /// Inverse standard deviations from <Rescale> section — multiplied with features
    pub scale: Vec<f32>,
}

impl CmvnStats {
    /// Parse a FunASR am.mvn file (Kaldi Nnet format).
    pub fn from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            AppError::Asr(format!(
                "Failed to read CMVN file {}: {}",
                path.display(),
                e
            ))
        })?;

        let mut shift = None;
        let mut scale = None;
        let mut expect_shift = false;
        let mut expect_scale = false;

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("<AddShift>") {
                expect_shift = true;
                continue;
            }
            if trimmed.starts_with("<Rescale>") {
                expect_scale = true;
                continue;
            }
            if expect_shift {
                shift = Some(Self::parse_vector_line(trimmed)?);
                expect_shift = false;
            } else if expect_scale {
                scale = Some(Self::parse_vector_line(trimmed)?);
                expect_scale = false;
            }
        }

        let shift = shift
            .ok_or_else(|| AppError::Asr("CMVN file missing <AddShift> section".to_string()))?;
        let scale = scale
            .ok_or_else(|| AppError::Asr("CMVN file missing <Rescale> section".to_string()))?;

        if shift.len() != scale.len() {
            return Err(AppError::Asr(format!(
                "CMVN shift/scale dimension mismatch: {} vs {}",
                shift.len(),
                scale.len()
            )));
        }

        log::info!(
            "CMVN loaded: dim={}, shift_range=[{:.3}, {:.3}], scale_range=[{:.3}, {:.3}]",
            shift.len(),
            shift.iter().cloned().reduce(f32::min).unwrap_or(0.0),
            shift.iter().cloned().reduce(f32::max).unwrap_or(0.0),
            scale.iter().cloned().reduce(f32::min).unwrap_or(0.0),
            scale.iter().cloned().reduce(f32::max).unwrap_or(0.0),
        );

        Ok(Self { shift, scale })
    }

    fn parse_vector_line(line: &str) -> Result<Vec<f32>> {
        let bracket_start = line
            .find('[')
            .ok_or_else(|| AppError::Asr(format!("CMVN vector line missing '[': {}", line)))?;
        let bracket_end = line
            .rfind(']')
            .ok_or_else(|| AppError::Asr(format!("CMVN vector line missing ']': {}", line)))?;

        let values_str = &line[bracket_start + 1..bracket_end];
        values_str
            .split_whitespace()
            .map(|s| {
                s.parse::<f32>()
                    .map_err(|e| AppError::Asr(format!("CMVN parse error '{}': {}", s, e)))
            })
            .collect()
    }

    /// Apply CMVN normalization in-place: feature[j] = (feature[j] + shift[j]) * scale[j]
    pub fn apply(&self, features: &mut [Vec<f32>]) {
        let dim = self.shift.len();
        for frame in features.iter_mut() {
            let frame_dim = frame.len().min(dim);
            for j in 0..frame_dim {
                frame[j] = (frame[j] + self.shift[j]) * self.scale[j];
            }
        }
    }
}

/// FBank feature extractor configuration
#[derive(Debug, Clone)]
pub struct FBankConfig {
    pub sample_rate: usize,
    pub frame_length_ms: usize,
    pub frame_shift_ms: usize,
    pub num_mel_bins: usize,
    pub preemphasis_coeff: f32,
    pub low_freq: f32,
    pub high_freq: f32,
    /// LFR (Low Frame Rate) configuration: stack m frames, skip n frames
    pub lfr: Option<(usize, usize)>,
}

impl Default for FBankConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16000,
            frame_length_ms: 25,
            frame_shift_ms: 10,
            num_mel_bins: 80,
            preemphasis_coeff: 0.97,
            low_freq: 20.0,
            high_freq: 7600.0,
            lfr: Some((7, 6)),
        }
    }
}

/// FBank feature extractor
pub struct FBankExtractor {
    config: FBankConfig,
    frame_length: usize,
    frame_shift: usize,
    fft_size: usize,
    window: Vec<f32>,
    mel_filters: Vec<Vec<f32>>,
    cmvn: Option<CmvnStats>,
}

impl FBankExtractor {
    pub fn new(config: FBankConfig) -> Result<Self> {
        let frame_length = config.sample_rate * config.frame_length_ms / 1000;
        let frame_shift = config.sample_rate * config.frame_shift_ms / 1000;
        let fft_size = next_power_of_2(frame_length);

        let window: Vec<f32> = (0..frame_length)
            .map(|i| {
                0.54 - 0.46
                    * (2.0 * std::f32::consts::PI * i as f32 / (frame_length - 1) as f32).cos()
            })
            .collect();

        let mel_filters = create_mel_filterbanks(
            fft_size,
            config.sample_rate,
            config.num_mel_bins,
            config.low_freq,
            config.high_freq,
        );

        Ok(Self {
            config,
            frame_length,
            frame_shift,
            fft_size,
            window,
            mel_filters,
            cmvn: None,
        })
    }

    pub fn set_cmvn(&mut self, cmvn: CmvnStats) {
        self.cmvn = Some(cmvn);
    }

    pub fn cmvn(&self) -> Option<&CmvnStats> {
        self.cmvn.as_ref()
    }

    pub fn lfr_config(&self) -> Option<(usize, usize)> {
        self.config.lfr
    }

    /// Extract raw FBank features only (no LFR, no CMVN).
    /// Returns 80-dim frames for accumulation in streaming mode.
    pub fn extract_fbank_only(&self, samples: &[f32]) -> Result<Vec<Vec<f32>>> {
        if samples.is_empty() {
            return Ok(vec![]);
        }

        let preemphasized = self.preemphasis(samples);
        if preemphasized.len() < self.frame_length {
            return Ok(vec![]);
        }

        let num_frames = (preemphasized.len() - self.frame_length) / self.frame_shift + 1;
        let mut features = Vec::with_capacity(num_frames);

        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(self.fft_size);
        let mut fft_input = vec![0.0f32; self.fft_size];
        let mut fft_output = fft.make_output_vec();

        for i in 0..num_frames {
            let start = i * self.frame_shift;
            let frame = &preemphasized[start..start + self.frame_length];

            fft_input.fill(0.0);
            for (j, (&s, &w)) in frame.iter().zip(self.window.iter()).enumerate() {
                fft_input[j] = s * w;
            }

            fft.process(&mut fft_input, &mut fft_output)
                .map_err(|e| AppError::Asr(format!("FFT failed: {}", e)))?;

            let power_spec: Vec<f32> = fft_output
                .iter()
                .map(|c| c.re * c.re + c.im * c.im)
                .collect();

            let mel_energies: Vec<f32> = self
                .mel_filters
                .iter()
                .map(|filter| {
                    filter
                        .iter()
                        .zip(power_spec.iter())
                        .map(|(f, p)| f * p)
                        .sum::<f32>()
                        .max(1e-10)
                })
                .map(|e| e.ln())
                .collect();

            features.push(mel_energies);
        }

        Ok(features)
    }

    /// Batch extract: FBank → LFR → CMVN (for non-streaming / whole-model use)
    pub fn extract(&self, samples: &[f32]) -> Result<Vec<Vec<f32>>> {
        let mut features = self.extract_fbank_only(samples)?;

        if let Some((m, n)) = self.config.lfr {
            features = apply_lfr(&features, m, n);
        }

        if let Some(ref cmvn) = self.cmvn {
            cmvn.apply(&mut features);
        }

        // Diagnostic: log final feature values once
        static LOGGED_FINAL: AtomicBool = AtomicBool::new(false);
        if !features.is_empty() && !LOGGED_FINAL.swap(true, Ordering::Relaxed) {
            let f = &features[0];
            log::info!(
                "[DIAG] batch extract frame[0]: dim={}, first5={:?}, cmvn_active={}",
                f.len(),
                &f[..5.min(f.len())],
                self.cmvn.is_some(),
            );
        }

        Ok(features)
    }

    fn preemphasis(&self, samples: &[f32]) -> Vec<f32> {
        let mut result = Vec::with_capacity(samples.len());
        result.push(samples[0]);
        for i in 1..samples.len() {
            result.push(samples[i] - self.config.preemphasis_coeff * samples[i - 1]);
        }
        result
    }

    pub fn frame_shift(&self) -> usize {
        self.frame_shift
    }

    pub fn num_mel_bins(&self) -> usize {
        self.config.num_mel_bins
    }
}

/// Apply LFR (Low Frame Rate) following FunASR reference implementation.
///
/// - Pads (m-1)/2 frames at the beginning by copying the first frame
/// - Pads at the end by copying the last frame if needed
/// - Output frame count: ceil(T / n)
pub(crate) fn apply_lfr(features: &[Vec<f32>], m: usize, n: usize) -> Vec<Vec<f32>> {
    if features.is_empty() || m == 0 {
        return features.to_vec();
    }

    let num_bins = features[0].len();
    let output_dim = num_bins * m;

    // Pad beginning: (m-1)/2 copies of the first frame
    let pad_begin = (m - 1) / 2;
    let mut padded = Vec::with_capacity(pad_begin + features.len() + m);
    for _ in 0..pad_begin {
        padded.push(features[0].clone());
    }
    padded.extend_from_slice(features);

    // Output frame count: ceil(original_T / n)
    let t_orig = features.len();
    let num_output = (t_orig + n - 1) / n;

    let mut result = Vec::with_capacity(num_output);
    for i in 0..num_output {
        let start = i * n;
        let mut stacked = Vec::with_capacity(output_dim);
        for j in 0..m {
            let idx = start + j;
            if idx < padded.len() {
                stacked.extend_from_slice(&padded[idx]);
            } else {
                stacked.extend_from_slice(padded.last().unwrap());
            }
        }
        result.push(stacked);
    }

    result
}

fn create_mel_filterbanks(
    fft_size: usize,
    sample_rate: usize,
    num_mel_bins: usize,
    low_freq: f32,
    high_freq: f32,
) -> Vec<Vec<f32>> {
    let num_fft_bins = fft_size / 2 + 1;

    let low_mel = hz_to_mel(low_freq);
    let high_mel = hz_to_mel(high_freq);

    let mel_points: Vec<f32> = (0..=num_mel_bins + 1)
        .map(|i| low_mel + i as f32 * (high_mel - low_mel) / (num_mel_bins + 1) as f32)
        .collect();

    let bin_indices: Vec<usize> = mel_points
        .iter()
        .map(|&m| mel_to_hz(m))
        .map(|f| ((fft_size as f32 * f) / sample_rate as f32).floor() as usize)
        .map(|b| b.min(num_fft_bins - 1))
        .collect();

    let mut filters = Vec::with_capacity(num_mel_bins);

    for i in 0..num_mel_bins {
        let mut filter = vec![0.0f32; num_fft_bins];
        let left = bin_indices[i];
        let center = bin_indices[i + 1];
        let right = bin_indices[i + 2];

        if center > left {
            for j in left..center {
                filter[j] = (j - left) as f32 / (center - left) as f32;
            }
        }
        if right > center {
            for j in center..right {
                filter[j] = (right - j) as f32 / (right - center) as f32;
            }
        }

        filters.push(filter);
    }

    filters
}

fn hz_to_mel(hz: f32) -> f32 {
    2595.0 * (1.0 + hz / 700.0).log10()
}

fn mel_to_hz(mel: f32) -> f32 {
    700.0 * (10f32.powf(mel / 2595.0) - 1.0)
}

fn next_power_of_2(n: usize) -> usize {
    let mut power = 1;
    while power < n {
        power *= 2;
    }
    power
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fbank_basic() {
        let extractor = FBankExtractor::new(FBankConfig::default()).unwrap();
        let samples: Vec<f32> = (0..16000).map(|i| (i as f32 * 0.01).sin()).collect();
        let features = extractor.extract(&samples).unwrap();
        assert!(!features.is_empty());
        assert_eq!(features[0].len(), 560);
    }

    #[test]
    fn test_fbank_no_lfr() {
        let config = FBankConfig {
            lfr: None,
            ..FBankConfig::default()
        };
        let extractor = FBankExtractor::new(config).unwrap();
        let samples: Vec<f32> = (0..16000).map(|i| (i as f32 * 0.01).sin()).collect();
        let features = extractor.extract(&samples).unwrap();
        assert!(features.len() > 90 && features.len() < 110);
        assert_eq!(features[0].len(), 80);
    }

    #[test]
    fn test_extract_fbank_only_returns_80dim() {
        let extractor = FBankExtractor::new(FBankConfig::default()).unwrap();
        let samples: Vec<f32> = (0..16000).map(|i| (i as f32 * 0.01).sin()).collect();
        let raw = extractor.extract_fbank_only(&samples).unwrap();
        assert!(raw.len() > 90 && raw.len() < 110);
        assert_eq!(raw[0].len(), 80);
    }

    #[test]
    fn test_mel_conversion() {
        let hz = 1000.0;
        let mel = hz_to_mel(hz);
        let hz_back = mel_to_hz(mel);
        assert!((hz - hz_back).abs() < 0.1);
    }

    #[test]
    fn test_lfr_padding() {
        let features: Vec<Vec<f32>> = (0..20).map(|i| vec![i as f32; 80]).collect();
        let result = apply_lfr(&features, 7, 6);
        assert_eq!(result.len(), 4); // ceil(20/6) = 4
        assert_eq!(result[0].len(), 560);
        assert_eq!(result[0][0], 0.0);
        assert_eq!(result[0][3 * 80], 0.0);
        assert_eq!(result[0][4 * 80], 1.0);
    }

    #[test]
    fn test_lfr_incremental_matches_batch() {
        // Simulate: 3 chunks of 61 raw frames, overlap 1 frame
        let all_frames: Vec<Vec<f32>> = (0..181).map(|i| vec![i as f32; 80]).collect();

        // Batch: apply LFR on all frames at once
        let batch_result = apply_lfr(&all_frames, 7, 6);

        // Incremental: simulate streaming with frame overlap dedup
        let chunk1: Vec<Vec<f32>> = all_frames[0..61].to_vec();
        let chunk2: Vec<Vec<f32>> = all_frames[60..121].to_vec(); // overlap 1
        let chunk3: Vec<Vec<f32>> = all_frames[120..181].to_vec(); // overlap 1

        let mut accumulated = chunk1;
        // Skip first frame of chunk2 (overlap)
        accumulated.extend_from_slice(&chunk2[1..]);
        // Skip first frame of chunk3 (overlap)
        accumulated.extend_from_slice(&chunk3[1..]);

        // accumulated should be frames 0..179 = 179 frames
        // But original is 181. The overlap dedup loses 2 frames.
        // Actually we need to NOT dedup but just accumulate correctly.
        // Let's verify the accumulated frames match the original
        assert_eq!(accumulated.len(), 181);

        let incremental_result = apply_lfr(&accumulated, 7, 6);
        assert_eq!(batch_result.len(), incremental_result.len());

        for (i, (b, inc)) in batch_result
            .iter()
            .zip(incremental_result.iter())
            .enumerate()
        {
            assert_eq!(b, inc, "LFR frame {} mismatch", i);
        }
    }

    #[test]
    fn test_cmvn_apply() {
        let cmvn = CmvnStats {
            shift: vec![-10.0, -20.0],
            scale: vec![0.1, 0.2],
        };
        let mut features = vec![vec![10.0, 20.0], vec![15.0, 25.0]];
        cmvn.apply(&mut features);
        assert!((features[0][0] - 0.0).abs() < 1e-6);
        assert!((features[0][1] - 0.0).abs() < 1e-6);
        assert!((features[1][0] - 0.5).abs() < 1e-6);
        assert!((features[1][1] - 1.0).abs() < 1e-6);
    }
}

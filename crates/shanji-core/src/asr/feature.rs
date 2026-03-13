//! Audio feature extraction for ASR
//!
//! Extracts FBank (Filter Bank) features from raw audio.
//! This is the standard input for Paraformer models.

use crate::error::Result;

/// FBank feature extractor configuration
#[derive(Debug, Clone)]
pub struct FBankConfig {
    /// Sample rate (must be 16000)
    pub sample_rate: usize,
    /// Frame length in milliseconds (typically 25ms)
    pub frame_length_ms: usize,
    /// Frame shift in milliseconds (typically 10ms)
    pub frame_shift_ms: usize,
    /// Number of mel filter banks (typically 80)
    pub num_mel_bins: usize,
    /// Pre-emphasis coefficient
    pub preemphasis_coeff: f32,
    /// Lower frequency cutoff
    pub low_freq: f32,
    /// Upper frequency cutoff
    pub high_freq: f32,
    /// LFR (Low Frame Rate) configuration: stack m frames, skip n frames
    /// If Some((m, n)), stack m frames to create m * num_mel_bins dimension
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
            lfr: Some((7, 6)), // Default: stack 7 frames, skip 6 -> 560 dim for Paraformer
        }
    }
}

/// FBank feature extractor
pub struct FBankExtractor {
    config: FBankConfig,
    /// Frame length in samples
    frame_length: usize,
    /// Frame shift in samples
    frame_shift: usize,
    /// FFT size (next power of 2 >= frame_length)
    fft_size: usize,
    /// Window function (Hamming)
    window: Vec<f32>,
    /// Mel filter banks
    mel_filters: Vec<Vec<f32>>,
}

impl FBankExtractor {
    /// Create new FBank extractor
    pub fn new(config: FBankConfig) -> Result<Self> {
        let frame_length = config.sample_rate * config.frame_length_ms / 1000;
        let frame_shift = config.sample_rate * config.frame_shift_ms / 1000;
        let fft_size = next_power_of_2(frame_length);

        // Create Hamming window
        let window: Vec<f32> = (0..frame_length)
            .map(|i| {
                0.54 - 0.46
                    * (2.0 * std::f32::consts::PI * i as f32 / (frame_length - 1) as f32).cos()
            })
            .collect();

        // Create mel filter banks
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
        })
    }

    /// Extract FBank features from audio samples
    ///
    /// Returns: (num_frames, num_mel_bins) features
    pub fn extract(&self, samples: &[f32]) -> Result<Vec<Vec<f32>>> {
        if samples.is_empty() {
            return Ok(vec![]);
        }

        // Apply pre-emphasis
        let preemphasized = self.preemphasis(samples);

        // Extract frames
        let num_frames = (preemphasized.len() - self.frame_length) / self.frame_shift + 1;
        let mut features = Vec::with_capacity(num_frames);

        for i in 0..num_frames {
            let start = i * self.frame_shift;
            let frame = &preemphasized[start..start + self.frame_length];

            // Apply window
            let windowed: Vec<f32> = frame
                .iter()
                .zip(self.window.iter())
                .map(|(s, w)| s * w)
                .collect();

            // Compute FFT
            let spectrum = compute_fft(&windowed, self.fft_size);

            // Compute power spectrum
            let power_spec: Vec<f32> = spectrum
                .chunks(2)
                .take(self.fft_size / 2 + 1)
                .map(|c| c[0] * c[0] + c[1] * c[1])
                .collect();

            // Apply mel filter banks
            let mel_energies: Vec<f32> = self
                .mel_filters
                .iter()
                .map(|filter| {
                    filter
                        .iter()
                        .zip(power_spec.iter())
                        .map(|(f, p)| f * p)
                        .sum::<f32>()
                        .max(1e-10) // Avoid log(0)
                })
                .map(|e| e.ln()) // Log compression
                .collect();

            features.push(mel_energies);
        }

        // Apply LFR (Low Frame Rate) processing if configured
        if let Some((m, n)) = self.config.lfr {
            log::info!("Applying LFR: stacking {} frames, skipping {} frames", m, n);
            let original_frames = features.len();
            features = Self::apply_lfr(&features, m, n);
            log::info!("LFR applied: {} frames -> {} frames, dim {} -> {}",
                original_frames, features.len(),
                self.config.num_mel_bins,
                if features.is_empty() { 0 } else { features[0].len() });
        } else {
            log::info!("LFR not configured, outputting raw {} dim features", self.config.num_mel_bins);
        }

        Ok(features)
    }

    /// Apply LFR (Low Frame Rate) processing
    /// Stack m consecutive frames into one frame, keeping every n-th frame
    fn apply_lfr(features: &[Vec<f32>], m: usize, n: usize) -> Vec<Vec<f32>> {
        if features.len() < m || m == 0 {
            return features.to_vec();
        }

        let num_frames = features.len();
        let num_bins = features[0].len();
        let output_dim = num_bins * m;
        let mut result = Vec::new();

        // Stack m frames, output every n frames
        for i in (0..=num_frames - m).step_by(n) {
            let mut stacked = Vec::with_capacity(output_dim);
            for j in 0..m {
                stacked.extend_from_slice(&features[i + j]);
            }
            result.push(stacked);
        }

        result
    }

    /// Apply pre-emphasis filter
    fn preemphasis(&self, samples: &[f32]) -> Vec<f32> {
        let mut result = Vec::with_capacity(samples.len());
        result.push(samples[0]);

        for i in 1..samples.len() {
            result.push(samples[i] - self.config.preemphasis_coeff * samples[i - 1]);
        }

        result
    }

    /// Get frame shift in samples
    pub fn frame_shift(&self) -> usize {
        self.frame_shift
    }

    /// Get number of mel bins
    pub fn num_mel_bins(&self) -> usize {
        self.config.num_mel_bins
    }
}

/// Compute FFT using real FFT (placeholder - should use rustfft for production)
fn compute_fft(input: &[f32], fft_size: usize) -> Vec<f32> {
    // Simple DFT implementation for placeholder
    // In production, use rustfft crate
    let mut output = vec![0.0f32; fft_size * 2];

    for k in 0..=fft_size / 2 {
        let mut real = 0.0f32;
        let mut imag = 0.0f32;

        for (n, &sample) in input.iter().enumerate() {
            let angle = -2.0 * std::f32::consts::PI * k as f32 * n as f32 / fft_size as f32;
            real += sample * angle.cos();
            imag += sample * angle.sin();
        }

        output[k * 2] = real;
        output[k * 2 + 1] = imag;

        if k > 0 && k < fft_size / 2 {
            output[(fft_size - k) * 2] = real;
            output[(fft_size - k) * 2 + 1] = -imag;
        }
    }

    output
}

/// Create mel filter banks
fn create_mel_filterbanks(
    fft_size: usize,
    sample_rate: usize,
    num_mel_bins: usize,
    low_freq: f32,
    high_freq: f32,
) -> Vec<Vec<f32>> {
    let num_fft_bins = fft_size / 2 + 1;

    // Convert Hz to Mel
    let low_mel = hz_to_mel(low_freq);
    let high_mel = hz_to_mel(high_freq);

    // Create mel points
    let mel_points: Vec<f32> = (0..=num_mel_bins + 1)
        .map(|i| low_mel + i as f32 * (high_mel - low_mel) / (num_mel_bins + 1) as f32)
        .collect();

    // Convert mel points back to Hz and then to FFT bin indices
    let bin_indices: Vec<usize> = mel_points
        .iter()
        .map(|&m| mel_to_hz(m))
        .map(|f| ((fft_size as f32 * f) / sample_rate as f32).floor() as usize)
        .map(|b| b.min(num_fft_bins - 1))
        .collect();

    // Create filter banks
    let mut filters = Vec::with_capacity(num_mel_bins);

    for i in 0..num_mel_bins {
        let mut filter = vec![0.0f32; num_fft_bins];

        let left = bin_indices[i];
        let center = bin_indices[i + 1];
        let right = bin_indices[i + 2];

        // Rising slope
        if center > left {
            for j in left..center {
                filter[j] = (j - left) as f32 / (center - left) as f32;
            }
        }

        // Falling slope
        if right > center {
            for j in center..right {
                filter[j] = (right - j) as f32 / (right - center) as f32;
            }
        }

        filters.push(filter);
    }

    filters
}

/// Convert Hz to Mel scale
fn hz_to_mel(hz: f32) -> f32 {
    2595.0 * (1.0 + hz / 700.0).log10()
}

/// Convert Mel to Hz
fn mel_to_hz(mel: f32) -> f32 {
    700.0 * (10f32.powf(mel / 2595.0) - 1.0)
}

/// Next power of 2
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

        // Create 1 second of audio
        let samples: Vec<f32> = (0..16000).map(|i| (i as f32 * 0.01).sin()).collect();

        let features = extractor.extract(&samples).unwrap();

        // Should have approximately 100 frames (1 second / 10ms shift)
        assert!(features.len() > 90 && features.len() < 110);

        // Each frame should have 80 mel bins
        assert_eq!(features[0].len(), 80);
    }

    #[test]
    fn test_mel_conversion() {
        // Test Hz <-> Mel conversion
        let hz = 1000.0;
        let mel = hz_to_mel(hz);
        let hz_back = mel_to_hz(mel);

        assert!((hz - hz_back).abs() < 0.1);
    }
}

use crate::asr::feature::{CmvnStats, FBankConfig, FBankExtractor};
use crate::error::{AppError, Result};
use ort::session::Session;
use ort::value::Tensor;
use serde::Deserialize;
use std::path::Path;

const MAX_VAD_FRAMES_PER_CHUNK: usize = 6_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsmnVadSegment {
    pub start_ms: u32,
    pub end_ms: u32,
    pub start_sample: usize,
    pub end_sample: usize,
}

pub struct FsmnVadSegmenter {
    session: Session,
    feature_extractor: FBankExtractor,
    config: VadConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct VadFileConfig {
    #[serde(default)]
    frontend_conf: FrontendConfig,
    #[serde(default)]
    model_conf: VadConfig,
    #[serde(default)]
    encoder_conf: EncoderConfig,
}

#[derive(Debug, Clone, Deserialize)]
struct FrontendConfig {
    #[serde(default = "default_sample_rate")]
    fs: usize,
    #[serde(default = "default_num_mels")]
    n_mels: usize,
    #[serde(default = "default_frame_length_ms")]
    frame_length: usize,
    #[serde(default = "default_frame_shift_ms")]
    frame_shift: usize,
    #[serde(default = "default_lfr_m")]
    lfr_m: usize,
    #[serde(default = "default_lfr_n")]
    lfr_n: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct EncoderConfig {
    #[serde(default = "default_fsmn_layers")]
    fsmn_layers: usize,
    #[serde(default = "default_proj_dim")]
    proj_dim: usize,
    #[serde(default = "default_lorder")]
    lorder: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct VadConfig {
    #[serde(default = "default_sample_rate")]
    sample_rate: usize,
    #[serde(default = "default_detect_mode")]
    detect_mode: i32,
    #[serde(default = "default_max_end_silence_time")]
    max_end_silence_time: i32,
    #[serde(default = "default_max_start_silence_time")]
    max_start_silence_time: i32,
    #[serde(default = "default_true")]
    do_start_point_detection: bool,
    #[serde(default = "default_true")]
    do_end_point_detection: bool,
    #[serde(default = "default_window_size_ms")]
    window_size_ms: i32,
    #[serde(default = "default_sil_to_speech_time")]
    sil_to_speech_time_thres: i32,
    #[serde(default = "default_speech_to_sil_time")]
    speech_to_sil_time_thres: i32,
    #[serde(default = "default_speech_to_noise_ratio")]
    speech_2_noise_ratio: f32,
    #[serde(default = "default_do_extend")]
    do_extend: i32,
    #[serde(default = "default_lookback_time_start_point")]
    lookback_time_start_point: i32,
    #[serde(default = "default_lookahead_time_end_point")]
    lookahead_time_end_point: i32,
    #[serde(default = "default_max_single_segment_time")]
    max_single_segment_time: i32,
    #[serde(default = "default_snr_thres")]
    snr_thres: f32,
    #[serde(default = "default_noise_frame_num_used_for_snr")]
    noise_frame_num_used_for_snr: i32,
    #[serde(default = "default_decibel_thres")]
    decibel_thres: f32,
    #[serde(default = "default_speech_noise_thres")]
    speech_noise_thres: f32,
    #[serde(default = "default_fe_prior_thres")]
    fe_prior_thres: f32,
    #[serde(default = "default_silence_pdf_num")]
    silence_pdf_num: usize,
    #[serde(default = "default_sil_pdf_ids")]
    sil_pdf_ids: Vec<usize>,
    #[serde(default = "default_false")]
    output_frame_probs: bool,
    #[serde(default = "default_frame_in_ms")]
    frame_in_ms: i32,
    #[serde(default = "default_frame_length_ms_i32")]
    frame_length_ms: i32,
}

impl Default for FrontendConfig {
    fn default() -> Self {
        Self {
            fs: default_sample_rate(),
            n_mels: default_num_mels(),
            frame_length: default_frame_length_ms(),
            frame_shift: default_frame_shift_ms(),
            lfr_m: default_lfr_m(),
            lfr_n: default_lfr_n(),
        }
    }
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            fsmn_layers: default_fsmn_layers(),
            proj_dim: default_proj_dim(),
            lorder: default_lorder(),
        }
    }
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            sample_rate: default_sample_rate(),
            detect_mode: default_detect_mode(),
            max_end_silence_time: default_max_end_silence_time(),
            max_start_silence_time: default_max_start_silence_time(),
            do_start_point_detection: default_true(),
            do_end_point_detection: default_true(),
            window_size_ms: default_window_size_ms(),
            sil_to_speech_time_thres: default_sil_to_speech_time(),
            speech_to_sil_time_thres: default_speech_to_sil_time(),
            speech_2_noise_ratio: default_speech_to_noise_ratio(),
            do_extend: default_do_extend(),
            lookback_time_start_point: default_lookback_time_start_point(),
            lookahead_time_end_point: default_lookahead_time_end_point(),
            max_single_segment_time: default_max_single_segment_time(),
            snr_thres: default_snr_thres(),
            noise_frame_num_used_for_snr: default_noise_frame_num_used_for_snr(),
            decibel_thres: default_decibel_thres(),
            speech_noise_thres: default_speech_noise_thres(),
            fe_prior_thres: default_fe_prior_thres(),
            silence_pdf_num: default_silence_pdf_num(),
            sil_pdf_ids: default_sil_pdf_ids(),
            output_frame_probs: default_false(),
            frame_in_ms: default_frame_in_ms(),
            frame_length_ms: default_frame_length_ms_i32(),
        }
    }
}

impl FsmnVadSegmenter {
    pub fn new(model_dir: &Path) -> Result<Self> {
        crate::ort_runtime::init_onnx_runtime()?;
        let config_path = model_dir.join("config.yaml");
        let model_path = model_dir.join("model_quant.onnx");
        let mvn_path = model_dir.join("am.mvn");

        let config_content = std::fs::read_to_string(&config_path).map_err(|e| {
            AppError::Asr(format!(
                "Failed to read FSMN VAD config {}: {}",
                config_path.display(),
                e
            ))
        })?;
        let config = serde_yaml::from_str::<VadFileConfig>(&config_content).map_err(|e| {
            AppError::Asr(format!(
                "Failed to parse FSMN VAD config {}: {}",
                config_path.display(),
                e
            ))
        })?;

        let session = Session::builder()
            .map_err(|e| AppError::Asr(format!("Failed to create FSMN VAD session: {}", e)))?
            .commit_from_file(&model_path)
            .map_err(|e| AppError::Asr(format!("Failed to load FSMN VAD model: {}", e)))?;

        let mut feature_extractor = FBankExtractor::new(FBankConfig {
            sample_rate: config.frontend_conf.fs,
            frame_length_ms: config.frontend_conf.frame_length,
            frame_shift_ms: config.frontend_conf.frame_shift,
            num_mel_bins: config.frontend_conf.n_mels,
            lfr: Some((config.frontend_conf.lfr_m, config.frontend_conf.lfr_n)),
            ..FBankConfig::default()
        })?;
        let cmvn = CmvnStats::from_file(&mvn_path)?;
        feature_extractor.set_cmvn(cmvn);

        Ok(Self {
            session,
            feature_extractor,
            config: config.model_conf,
        })
    }

    pub fn segment(&mut self, samples: &[f32]) -> Result<Vec<FsmnVadSegment>> {
        if samples.is_empty() {
            return Ok(Vec::new());
        }

        let features = self.feature_extractor.extract(samples)?;
        if features.is_empty() {
            return Ok(Vec::new());
        }

        let scores = self.infer_scores(&features)?;
        let decibels = compute_decibels(
            samples,
            self.config.sample_rate,
            self.config.frame_length_ms as usize,
            self.config.frame_in_ms as usize,
        );
        let mut tracker = VadStateTracker::new(self.config.clone());

        for (frame_index, score_row) in scores.iter().enumerate() {
            let frame_state =
                tracker.classify_frame(score_row, *decibels.get(frame_index).unwrap_or(&0.0));
            tracker.step(frame_state, frame_index, frame_index + 1 == scores.len());
        }

        Ok(tracker
            .segments()
            .into_iter()
            .filter_map(|(start_ms, end_ms)| {
                let start_sample = ms_to_samples(start_ms, self.config.sample_rate);
                let end_sample = ms_to_samples(end_ms, self.config.sample_rate).min(samples.len());
                (end_sample > start_sample).then_some(FsmnVadSegment {
                    start_ms,
                    end_ms,
                    start_sample,
                    end_sample,
                })
            })
            .collect())
    }

    fn infer_scores(&mut self, features: &[Vec<f32>]) -> Result<Vec<Vec<f32>>> {
        let feature_dim = features
            .first()
            .map(Vec::len)
            .ok_or_else(|| AppError::Asr("FSMN VAD features were empty".to_string()))?;
        let cache_count = 4usize;
        let cache_shape = [
            1usize,
            default_proj_dim(),
            default_lorder().saturating_sub(1),
            1usize,
        ];
        let cache_size: usize = cache_shape.iter().product();
        let mut caches = vec![vec![0.0f32; cache_size]; cache_count];
        let mut all_scores = Vec::with_capacity(features.len());
        let class_count = 248usize;

        let mut start = 0usize;
        while start < features.len() {
            let end = (start + MAX_VAD_FRAMES_PER_CHUNK).min(features.len());
            let chunk = &features[start..end];
            let flattened = chunk
                .iter()
                .flat_map(|frame| frame.iter().copied())
                .collect::<Vec<_>>();

            let speech_tensor = Tensor::from_array((
                [1usize, chunk.len(), feature_dim],
                flattened.into_boxed_slice(),
            ))
            .map_err(|e| AppError::Asr(format!("Failed to build FSMN VAD speech tensor: {}", e)))?;

            let in_cache0 = Tensor::from_array((cache_shape, caches[0].clone().into_boxed_slice()))
                .map_err(|e| {
                    AppError::Asr(format!("Failed to build FSMN VAD cache tensor: {}", e))
                })?;
            let in_cache1 = Tensor::from_array((cache_shape, caches[1].clone().into_boxed_slice()))
                .map_err(|e| {
                    AppError::Asr(format!("Failed to build FSMN VAD cache tensor: {}", e))
                })?;
            let in_cache2 = Tensor::from_array((cache_shape, caches[2].clone().into_boxed_slice()))
                .map_err(|e| {
                    AppError::Asr(format!("Failed to build FSMN VAD cache tensor: {}", e))
                })?;
            let in_cache3 = Tensor::from_array((cache_shape, caches[3].clone().into_boxed_slice()))
                .map_err(|e| {
                    AppError::Asr(format!("Failed to build FSMN VAD cache tensor: {}", e))
                })?;

            let outputs = self
                .session
                .run(ort::inputs! {
                    "speech" => speech_tensor,
                    "in_cache0" => in_cache0,
                    "in_cache1" => in_cache1,
                    "in_cache2" => in_cache2,
                    "in_cache3" => in_cache3,
                })
                .map_err(|e| AppError::Asr(format!("FSMN VAD inference failed: {}", e)))?;

            let (_, logits) = outputs["logits"]
                .try_extract_tensor::<f32>()
                .map_err(|e| AppError::Asr(format!("Failed to extract FSMN VAD logits: {}", e)))?;
            let logits = logits.to_vec();
            for frame_index in 0..chunk.len() {
                let offset = frame_index * class_count;
                let end = offset + class_count;
                all_scores.push(logits[offset..end.min(logits.len())].to_vec());
            }

            for (cache_index, cache) in caches.iter_mut().enumerate() {
                let output_name = format!("out_cache{}", cache_index);
                let (_, out_cache) = outputs[output_name.as_str()]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| {
                        AppError::Asr(format!(
                            "Failed to extract FSMN VAD cache {}: {}",
                            output_name, e
                        ))
                    })?;
                *cache = out_cache.to_vec();
            }

            start = end;
        }

        Ok(all_scores)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FrameState {
    Speech,
    Silence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AudioChangeState {
    SpeechToSpeech,
    SpeechToSilence,
    SilenceToSilence,
    SilenceToSpeech,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VadStateMachine {
    StartPointNotDetected,
    InSpeechSegment,
    EndPointDetected,
}

struct WindowDetector {
    win_state: Vec<i32>,
    cur_pos: usize,
    win_sum: i32,
    sil_to_speech_threshold: i32,
    speech_to_sil_threshold: i32,
    previous_state: FrameState,
}

impl WindowDetector {
    fn new(config: &VadConfig) -> Self {
        let win_size_frame = (config.window_size_ms / config.frame_in_ms).max(1) as usize;
        Self {
            win_state: vec![0; win_size_frame],
            cur_pos: 0,
            win_sum: 0,
            sil_to_speech_threshold: (config.sil_to_speech_time_thres / config.frame_in_ms).max(1),
            speech_to_sil_threshold: (config.speech_to_sil_time_thres / config.frame_in_ms).max(0),
            previous_state: FrameState::Silence,
        }
    }

    fn reset(&mut self) {
        self.cur_pos = 0;
        self.win_sum = 0;
        self.win_state.fill(0);
        self.previous_state = FrameState::Silence;
    }

    fn window_size(&self) -> usize {
        self.win_state.len()
    }

    fn detect_one_frame(&mut self, frame_state: FrameState) -> AudioChangeState {
        let current = matches!(frame_state, FrameState::Speech) as i32;
        self.win_sum -= self.win_state[self.cur_pos];
        self.win_sum += current;
        self.win_state[self.cur_pos] = current;
        self.cur_pos = (self.cur_pos + 1) % self.win_state.len();

        if self.previous_state == FrameState::Silence
            && self.win_sum >= self.sil_to_speech_threshold
        {
            self.previous_state = FrameState::Speech;
            return AudioChangeState::SilenceToSpeech;
        }

        if self.previous_state == FrameState::Speech && self.win_sum <= self.speech_to_sil_threshold
        {
            self.previous_state = FrameState::Silence;
            return AudioChangeState::SpeechToSilence;
        }

        match self.previous_state {
            FrameState::Speech => AudioChangeState::SpeechToSpeech,
            FrameState::Silence => AudioChangeState::SilenceToSilence,
        }
    }
}

struct VadStateTracker {
    config: VadConfig,
    window_detector: WindowDetector,
    state_machine: VadStateMachine,
    continuous_silence_frame_count: usize,
    latest_confirmed_speech_frame: usize,
    last_confirmed_silence_frame: Option<usize>,
    confirmed_start_frame: Option<usize>,
    number_end_time_detected: usize,
    max_end_sil_frame_cnt_thresh: usize,
    noise_average_decibel: f32,
    segments_ms: Vec<(u32, u32)>,
}

impl VadStateTracker {
    fn new(config: VadConfig) -> Self {
        let max_end_sil_frame_cnt_thresh =
            (config.max_end_silence_time - config.speech_to_sil_time_thres).max(0) as usize;
        Self {
            window_detector: WindowDetector::new(&config),
            config,
            state_machine: VadStateMachine::StartPointNotDetected,
            continuous_silence_frame_count: 0,
            latest_confirmed_speech_frame: 0,
            last_confirmed_silence_frame: None,
            confirmed_start_frame: None,
            number_end_time_detected: 0,
            max_end_sil_frame_cnt_thresh,
            noise_average_decibel: -100.0,
            segments_ms: Vec::new(),
        }
    }

    fn classify_frame(&mut self, score_row: &[f32], current_decibel: f32) -> FrameState {
        if current_decibel < self.config.decibel_thres {
            return FrameState::Silence;
        }

        let silence_score = self
            .config
            .sil_pdf_ids
            .iter()
            .filter_map(|index| score_row.get(*index))
            .copied()
            .sum::<f32>()
            .clamp(1e-6, 1.0 - 1e-6);
        let speech_score = (1.0 - silence_score).clamp(1e-6, 1.0 - 1e-6);
        let noise_prob = silence_score.ln() * self.config.speech_2_noise_ratio;
        let speech_prob = speech_score.ln();
        let current_snr = current_decibel - self.noise_average_decibel;

        let is_speech = speech_prob.exp() >= noise_prob.exp() + self.config.speech_noise_thres
            && current_snr >= self.config.snr_thres
            && current_decibel >= self.config.decibel_thres;

        if is_speech {
            FrameState::Speech
        } else {
            if self.noise_average_decibel < -99.9 {
                self.noise_average_decibel = current_decibel;
            } else {
                self.noise_average_decibel = (current_decibel
                    + self.noise_average_decibel
                        * (self.config.noise_frame_num_used_for_snr.max(1) - 1) as f32)
                    / self.config.noise_frame_num_used_for_snr.max(1) as f32;
            }
            FrameState::Silence
        }
    }

    fn step(&mut self, frame_state: FrameState, frame_index: usize, is_final_frame: bool) {
        let state_change = self.window_detector.detect_one_frame(frame_state);
        let frame_shift_ms = self.config.frame_in_ms.max(1) as usize;
        let max_single_segment_frames =
            (self.config.max_single_segment_time.max(0) as usize) / frame_shift_ms;
        let lookahead_frames =
            (self.config.lookahead_time_end_point.max(0) as usize) / frame_shift_ms;

        match state_change {
            AudioChangeState::SilenceToSpeech => {
                self.continuous_silence_frame_count = 0;
                if self.state_machine == VadStateMachine::StartPointNotDetected {
                    let start_frame = frame_index.saturating_sub(self.latency_frames_at_start());
                    self.confirmed_start_frame = Some(start_frame);
                    self.latest_confirmed_speech_frame = frame_index;
                    self.state_machine = VadStateMachine::InSpeechSegment;
                } else if self.state_machine == VadStateMachine::InSpeechSegment {
                    self.latest_confirmed_speech_frame = frame_index;
                    if let Some(start_frame) = self.confirmed_start_frame {
                        if frame_index.saturating_sub(start_frame) + 1 > max_single_segment_frames {
                            self.finish_segment(frame_index);
                        }
                    }
                }
            }
            AudioChangeState::SpeechToSilence => {
                self.continuous_silence_frame_count = 0;
                if self.state_machine == VadStateMachine::InSpeechSegment {
                    if let Some(start_frame) = self.confirmed_start_frame {
                        if frame_index.saturating_sub(start_frame) + 1 > max_single_segment_frames {
                            self.finish_segment(frame_index);
                        } else if !is_final_frame {
                            self.latest_confirmed_speech_frame = frame_index;
                        } else {
                            self.finish_segment(frame_index);
                        }
                    }
                }
            }
            AudioChangeState::SpeechToSpeech => {
                self.continuous_silence_frame_count = 0;
                if self.state_machine == VadStateMachine::InSpeechSegment {
                    if let Some(start_frame) = self.confirmed_start_frame {
                        if frame_index.saturating_sub(start_frame) + 1 > max_single_segment_frames {
                            self.finish_segment(frame_index);
                        } else if !is_final_frame {
                            self.latest_confirmed_speech_frame = frame_index;
                        } else {
                            self.finish_segment(frame_index);
                        }
                    }
                }
            }
            AudioChangeState::SilenceToSilence => {
                self.continuous_silence_frame_count += 1;
                match self.state_machine {
                    VadStateMachine::StartPointNotDetected => {
                        if frame_index >= self.latency_frames_at_start() {
                            self.last_confirmed_silence_frame =
                                Some(frame_index - self.latency_frames_at_start());
                        }
                    }
                    VadStateMachine::InSpeechSegment => {
                        if self.continuous_silence_frame_count >= self.max_end_sil_frame_cnt_thresh
                        {
                            let mut lookback_frames = self
                                .max_end_sil_frame_cnt_thresh
                                .saturating_sub(lookahead_frames);
                            if self.config.do_extend != 0 {
                                lookback_frames = lookback_frames.saturating_sub(1);
                            }
                            let end_frame = frame_index.saturating_sub(lookback_frames);
                            self.finish_segment(end_frame);
                        } else if let Some(start_frame) = self.confirmed_start_frame {
                            if frame_index.saturating_sub(start_frame) + 1
                                > max_single_segment_frames
                            {
                                self.finish_segment(frame_index);
                            } else if self.config.do_extend != 0
                                && !is_final_frame
                                && self.continuous_silence_frame_count <= lookahead_frames
                            {
                                self.latest_confirmed_speech_frame = frame_index;
                            } else if is_final_frame {
                                self.finish_segment(frame_index);
                            }
                        }
                    }
                    VadStateMachine::EndPointDetected => {}
                }
            }
        }

        if self.state_machine == VadStateMachine::EndPointDetected
            && self.config.detect_mode == default_detect_mode()
        {
            self.reset_for_next_segment();
        }
    }

    fn segments(self) -> Vec<(u32, u32)> {
        self.segments_ms
    }

    fn latency_frames_at_start(&self) -> usize {
        let mut latency = self.window_detector.window_size();
        if self.config.do_extend != 0 {
            latency += (self.config.lookback_time_start_point.max(0)
                / self.config.frame_in_ms.max(1)) as usize;
        }
        latency
    }

    fn finish_segment(&mut self, end_frame: usize) {
        let Some(start_frame) = self.confirmed_start_frame else {
            return;
        };
        let end_frame = end_frame.max(start_frame);
        let start_ms = (start_frame * self.config.frame_in_ms.max(1) as usize) as u32;
        let end_ms = ((end_frame + 1) * self.config.frame_in_ms.max(1) as usize) as u32;
        self.segments_ms.push((start_ms, end_ms));
        self.number_end_time_detected += 1;
        self.state_machine = VadStateMachine::EndPointDetected;
    }

    fn reset_for_next_segment(&mut self) {
        self.state_machine = VadStateMachine::StartPointNotDetected;
        self.continuous_silence_frame_count = 0;
        self.latest_confirmed_speech_frame = 0;
        self.last_confirmed_silence_frame = None;
        self.confirmed_start_frame = None;
        self.window_detector.reset();
    }
}

fn compute_decibels(
    waveform: &[f32],
    sample_rate: usize,
    frame_length_ms: usize,
    frame_shift_ms: usize,
) -> Vec<f32> {
    let frame_sample_length = sample_rate * frame_length_ms / 1_000usize;
    let frame_shift_length = sample_rate * frame_shift_ms / 1_000usize;
    if waveform.len() < frame_sample_length || frame_sample_length == 0 || frame_shift_length == 0 {
        return Vec::new();
    }

    let mut result = Vec::new();
    for offset in (0..=waveform.len() - frame_sample_length).step_by(frame_shift_length) {
        let frame = &waveform[offset..offset + frame_sample_length];
        let energy = frame.iter().map(|sample| sample * sample).sum::<f32>();
        result.push(10.0 * (energy + 1e-6).log10());
    }
    result
}

fn ms_to_samples(ms: u32, sample_rate: usize) -> usize {
    (ms as usize * sample_rate) / 1_000usize
}

fn default_sample_rate() -> usize {
    16_000
}

fn default_num_mels() -> usize {
    80
}

fn default_frame_length_ms() -> usize {
    25
}

fn default_frame_shift_ms() -> usize {
    10
}

fn default_lfr_m() -> usize {
    5
}

fn default_lfr_n() -> usize {
    1
}

fn default_fsmn_layers() -> usize {
    4
}

fn default_proj_dim() -> usize {
    128
}

fn default_lorder() -> usize {
    20
}

fn default_detect_mode() -> i32 {
    1
}

fn default_max_end_silence_time() -> i32 {
    800
}

fn default_max_start_silence_time() -> i32 {
    3_000
}

fn default_true() -> bool {
    true
}

fn default_false() -> bool {
    false
}

fn default_window_size_ms() -> i32 {
    200
}

fn default_sil_to_speech_time() -> i32 {
    150
}

fn default_speech_to_sil_time() -> i32 {
    150
}

fn default_speech_to_noise_ratio() -> f32 {
    1.0
}

fn default_do_extend() -> i32 {
    1
}

fn default_lookback_time_start_point() -> i32 {
    200
}

fn default_lookahead_time_end_point() -> i32 {
    100
}

fn default_max_single_segment_time() -> i32 {
    20_000
}

fn default_snr_thres() -> f32 {
    -100.0
}

fn default_noise_frame_num_used_for_snr() -> i32 {
    100
}

fn default_decibel_thres() -> f32 {
    -100.0
}

fn default_speech_noise_thres() -> f32 {
    0.6
}

fn default_fe_prior_thres() -> f32 {
    0.0001
}

fn default_silence_pdf_num() -> usize {
    1
}

fn default_sil_pdf_ids() -> Vec<usize> {
    vec![0]
}

fn default_frame_in_ms() -> i32 {
    10
}

fn default_frame_length_ms_i32() -> i32 {
    25
}

#[cfg(test)]
mod tests {
    use super::{compute_decibels, AudioChangeState, FrameState, VadConfig, WindowDetector};

    #[test]
    fn window_detector_switches_after_threshold() {
        let config = VadConfig::default();
        let mut detector = WindowDetector::new(&config);
        let mut states = Vec::new();
        for _ in 0..20 {
            states.push(detector.detect_one_frame(FrameState::Speech));
        }
        assert!(states.contains(&AudioChangeState::SilenceToSpeech));
    }

    #[test]
    fn compute_decibels_produces_frames() {
        let samples = vec![0.1f32; 16_000];
        let decibels = compute_decibels(&samples, 16_000, 25, 10);
        assert!(!decibels.is_empty());
    }
}

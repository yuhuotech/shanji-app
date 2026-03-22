//! Silero VAD v5 语音活动检测
//!
//! 输入：16kHz，512 samples/帧（32ms）
//! 输出：VadEvent（Speech 含可送入 ASR 的 samples，Silence 丢弃）
//!
//! 状态机：Silence → SpeechStarting → Speaking → SpeechEnding → Silence
//! - 前置缓冲 384ms（约 12 帧），给弱起音更多回放空间
//! - 尾部拖尾由 silence_timeout_ms 控制，防止字尾被截断

use crate::error::{AppError, Result};
use ndarray::Array3;
use ort::session::Session;
use ort::value::Tensor;
use std::collections::VecDeque;
use std::path::Path;

/// 内置的 Silero VAD v5 ONNX 模型字节（随二进制一起打包，无需下载）
static VAD_MODEL_BYTES: &[u8] = include_bytes!("../assets/silero_vad.onnx");

/// 确保 VAD 模型文件存在于 `target_path`，若不存在则从内置字节写入
pub fn ensure_vad_model(target_path: &Path) -> Result<()> {
    if target_path.exists() {
        return Ok(());
    }
    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AppError::Asr(format!("Failed to create VAD model dir: {}", e)))?;
    }
    std::fs::write(target_path, VAD_MODEL_BYTES)
        .map_err(|e| AppError::Asr(format!("Failed to write VAD model: {}", e)))?;
    log::info!("VAD model written to {:?}", target_path);
    Ok(())
}

const FRAME_SIZE: usize = 512;
const PRE_BUFFER_FRAMES: usize = 12; // 384ms
const SAMPLE_RATE: i64 = 16000;

#[derive(Debug)]
pub enum VadEvent {
    /// 语音帧，携带需送入 ASR 的 samples（含回放的前置缓冲帧）
    Speech {
        samples: Vec<f32>,
        leading_pause_ms: u32,
    },
    /// 静音帧
    Silence,
}

#[derive(Debug, PartialEq)]
enum VadState {
    Silence,
    SpeechStarting,
    Speaking,
    SpeechEnding,
}

pub struct VadDetector {
    session: Session,
    start_threshold: f32,
    end_threshold: f32,
    min_speech_frames: usize,
    /// Silero VAD v5 RNN state，shape [2, 1, 128]
    rnn_state: Array3<f32>,
    vad_state: VadState,
    /// 前置缓冲，保存最近 PRE_BUFFER_FRAMES 帧
    pre_buffer: VecDeque<Vec<f32>>,
    startup_buffer: Vec<Vec<f32>>,
    speech_frames: usize,
    pause_frames: usize,
    tail_remaining: usize,
    tail_frames: usize,
    frame_buffer: Vec<f32>,
}

impl VadDetector {
    pub fn new(
        model_path: &Path,
        start_threshold: f32,
        end_threshold: f32,
        min_speech_frames: u32,
        silence_timeout_ms: u32,
    ) -> Result<Self> {
        crate::ort_runtime::init_onnx_runtime()?;
        let session = Session::builder()
            .map_err(|e| AppError::Asr(format!("VAD session builder failed: {}", e)))?
            .commit_from_file(model_path)
            .map_err(|e| AppError::Asr(format!("Failed to load VAD model: {}", e)))?;
        let tail_frames = ((silence_timeout_ms as f32) / 32.0).round().max(1.0) as usize;

        Ok(Self {
            session,
            start_threshold,
            end_threshold: end_threshold.min(start_threshold),
            min_speech_frames: min_speech_frames.max(1) as usize,
            rnn_state: Array3::<f32>::zeros((2, 1, 128)),
            vad_state: VadState::Silence,
            pre_buffer: VecDeque::with_capacity(PRE_BUFFER_FRAMES + 1),
            startup_buffer: Vec::with_capacity(PRE_BUFFER_FRAMES + 4),
            speech_frames: 0,
            pause_frames: 0,
            tail_remaining: 0,
            tail_frames,
            frame_buffer: Vec::with_capacity(FRAME_SIZE * 2),
        })
    }

    /// 输入 16kHz samples（任意长度），返回 VadEvent 列表
    pub fn process(&mut self, samples: &[f32]) -> Result<Vec<VadEvent>> {
        self.frame_buffer.extend_from_slice(samples);
        let mut events = Vec::new();

        while self.frame_buffer.len() >= FRAME_SIZE {
            let frame: Vec<f32> = self.frame_buffer.drain(..FRAME_SIZE).collect();
            let event = self.process_frame(frame)?;
            events.push(event);
        }

        Ok(events)
    }

    fn process_frame(&mut self, frame: Vec<f32>) -> Result<VadEvent> {
        let prob = self.infer(&frame)?;

        match self.vad_state {
            VadState::Silence => {
                self.push_pre_buffer(frame);

                if prob >= self.start_threshold {
                    self.vad_state = VadState::SpeechStarting;
                    self.speech_frames = 1;
                    self.startup_buffer = self.pre_buffer.iter().cloned().collect();
                    log::debug!(
                        "VAD speech start armed: prob={:.3}, preroll_frames={}",
                        prob,
                        self.pre_buffer.len()
                    );

                    if self.min_speech_frames <= 1 {
                        self.vad_state = VadState::Speaking;
                        return Ok(VadEvent::Speech {
                            samples: self.take_startup_speech(self.speech_frames, prob),
                            leading_pause_ms: 0,
                        });
                    }

                    Ok(VadEvent::Silence)
                } else {
                    Ok(VadEvent::Silence)
                }
            }

            VadState::SpeechStarting => {
                if prob >= self.end_threshold {
                    self.speech_frames += 1;
                    self.startup_buffer.push(frame);

                    if self.speech_frames >= self.min_speech_frames {
                        self.vad_state = VadState::Speaking;
                        Ok(VadEvent::Speech {
                            samples: self.take_startup_speech(self.speech_frames, prob),
                            leading_pause_ms: 0,
                        })
                    } else {
                        Ok(VadEvent::Silence)
                    }
                } else {
                    self.vad_state = VadState::Silence;
                    self.speech_frames = 0;
                    self.startup_buffer.clear();
                    self.pre_buffer.clear();
                    self.push_pre_buffer(frame);
                    Ok(VadEvent::Silence)
                }
            }

            VadState::Speaking => {
                if prob < self.end_threshold {
                    self.vad_state = VadState::SpeechEnding;
                    self.tail_remaining = self.tail_frames;
                    self.pause_frames = 1;
                }
                Ok(VadEvent::Speech {
                    samples: frame,
                    leading_pause_ms: 0,
                })
            }

            VadState::SpeechEnding => {
                if prob >= self.start_threshold {
                    let leading_pause_ms = (self.pause_frames as u32) * 32;
                    self.vad_state = VadState::Speaking;
                    self.pause_frames = 0;
                    return Ok(VadEvent::Speech {
                        samples: frame,
                        leading_pause_ms,
                    });
                }

                self.pause_frames += 1;
                if self.tail_remaining > 0 {
                    self.tail_remaining -= 1;
                    Ok(VadEvent::Speech {
                        samples: frame,
                        leading_pause_ms: 0,
                    })
                } else {
                    log::debug!("VAD segment boundary reached, resetting recurrent state");
                    self.reset_segment_state();
                    self.push_pre_buffer(frame);
                    Ok(VadEvent::Silence)
                }
            }
        }
    }

    fn push_pre_buffer(&mut self, frame: Vec<f32>) {
        if self.pre_buffer.len() >= PRE_BUFFER_FRAMES {
            self.pre_buffer.pop_front();
        }
        self.pre_buffer.push_back(frame);
    }

    fn take_startup_speech(&mut self, confirmed_frames: usize, prob: f32) -> Vec<f32> {
        let replay_samples = self.startup_buffer.iter().map(Vec::len).sum::<usize>();
        log::info!(
            "VAD speech confirmed: prob={:.3}, confirm_frames={}, replay_samples={}",
            prob,
            confirmed_frames,
            replay_samples
        );
        let mut speech = Vec::new();
        for buf in self.startup_buffer.drain(..) {
            speech.extend(buf);
        }
        self.pre_buffer.clear();
        self.speech_frames = 0;
        speech
    }

    fn reset_segment_state(&mut self) {
        self.rnn_state = Array3::<f32>::zeros((2, 1, 128));
        self.vad_state = VadState::Silence;
        self.pre_buffer.clear();
        self.startup_buffer.clear();
        self.speech_frames = 0;
        self.pause_frames = 0;
        self.tail_remaining = 0;
    }

    fn infer(&mut self, frame: &[f32]) -> Result<f32> {
        // input: [1, 512] f32
        let input_tensor =
            Tensor::from_array(([1usize, FRAME_SIZE], frame.to_vec().into_boxed_slice()))
                .map_err(|e| AppError::Asr(format!("VAD input tensor failed: {}", e)))?;

        // state: [2, 1, 128] f32
        let state_data = self.rnn_state.clone().into_raw_vec();
        let state_tensor = Tensor::from_array(([2usize, 1, 128], state_data.into_boxed_slice()))
            .map_err(|e| AppError::Asr(format!("VAD state tensor failed: {}", e)))?;

        // sr: [1] i64
        let sr_tensor = Tensor::from_array(([1usize], vec![SAMPLE_RATE].into_boxed_slice()))
            .map_err(|e| AppError::Asr(format!("VAD sr tensor failed: {}", e)))?;

        let outputs = self
            .session
            .run(ort::inputs! {
                "input" => input_tensor,
                "state" => state_tensor,
                "sr" => sr_tensor,
            })
            .map_err(|e| AppError::Asr(format!("VAD inference failed: {}", e)))?;

        // 更新 RNN state: stateN shape [2, 1, 128]
        let (_, new_state_data) = outputs["stateN"]
            .try_extract_tensor::<f32>()
            .map_err(|e| AppError::Asr(format!("VAD stateN extraction failed: {}", e)))?;
        self.rnn_state = Array3::from_shape_vec((2, 1, 128), new_state_data.to_vec())
            .map_err(|e| AppError::Asr(format!("VAD state reshape failed: {}", e)))?;

        // 提取语音概率: output shape [1, 1]
        let (_, prob_data) = outputs["output"]
            .try_extract_tensor::<f32>()
            .map_err(|e| AppError::Asr(format!("VAD output extraction failed: {}", e)))?;

        Ok(prob_data.iter().next().copied().unwrap_or(0.0))
    }

    pub fn reset(&mut self) {
        self.reset_segment_state();
        self.frame_buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_splitting_logic() {
        let mut buf: Vec<f32> = Vec::new();
        buf.extend_from_slice(&vec![0.0f32; 1024]);
        let frames: Vec<Vec<f32>> = buf
            .chunks(FRAME_SIZE)
            .filter(|c| c.len() == FRAME_SIZE)
            .map(|c| c.to_vec())
            .collect();
        assert_eq!(frames.len(), 2);
    }

    #[test]
    fn test_pre_buffer_capacity() {
        // pre_buffer 最多保存 PRE_BUFFER_FRAMES 帧
        let mut buf: VecDeque<Vec<f32>> = VecDeque::with_capacity(PRE_BUFFER_FRAMES + 1);
        for _ in 0..PRE_BUFFER_FRAMES + 3 {
            if buf.len() >= PRE_BUFFER_FRAMES {
                buf.pop_front();
            }
            buf.push_back(vec![0.0f32; FRAME_SIZE]);
        }
        assert!(buf.len() <= PRE_BUFFER_FRAMES);
    }
}

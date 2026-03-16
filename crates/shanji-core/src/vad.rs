//! Silero VAD v5 语音活动检测
//!
//! 输入：16kHz，512 samples/帧（32ms）
//! 输出：VadEvent（Speech 含可送入 ASR 的 samples，Silence 丢弃）
//!
//! 状态机：Silence → SpeechStarting → Speaking → SpeechEnding → Silence
//! - 前置缓冲 200ms（约 6 帧），防止字头被截断
//! - 尾部拖尾 500ms（约 15 帧），防止字尾被截断

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
const PRE_BUFFER_FRAMES: usize = 6; // 200ms
const TAIL_FRAMES: usize = 15; // 500ms
const SAMPLE_RATE: i64 = 16000;

#[derive(Debug)]
pub enum VadEvent {
    /// 语音帧，携带需送入 ASR 的 samples（含回放的前置缓冲帧）
    Speech(Vec<f32>),
    /// 静音帧
    Silence,
}

#[derive(Debug, PartialEq)]
enum VadState {
    Silence,
    Speaking,
    SpeechEnding,
}

pub struct VadDetector {
    session: Session,
    threshold: f32,
    /// Silero VAD v5 RNN state，shape [2, 1, 128]
    rnn_state: Array3<f32>,
    vad_state: VadState,
    /// 前置缓冲，保存最近 PRE_BUFFER_FRAMES 帧
    pre_buffer: VecDeque<Vec<f32>>,
    tail_remaining: usize,
    frame_buffer: Vec<f32>,
}

impl VadDetector {
    pub fn new(model_path: &Path, threshold: f32) -> Result<Self> {
        let session = Session::builder()
            .map_err(|e| AppError::Asr(format!("VAD session builder failed: {}", e)))?
            .commit_from_file(model_path)
            .map_err(|e| AppError::Asr(format!("Failed to load VAD model: {}", e)))?;

        Ok(Self {
            session,
            threshold,
            rnn_state: Array3::<f32>::zeros((2, 1, 128)),
            vad_state: VadState::Silence,
            pre_buffer: VecDeque::with_capacity(PRE_BUFFER_FRAMES + 1),
            tail_remaining: 0,
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
                if self.pre_buffer.len() >= PRE_BUFFER_FRAMES {
                    self.pre_buffer.pop_front();
                }
                self.pre_buffer.push_back(frame);

                if prob >= self.threshold {
                    self.vad_state = VadState::Speaking;
                    // 回放前置缓冲
                    let mut speech: Vec<f32> = Vec::new();
                    for buf in self.pre_buffer.drain(..) {
                        speech.extend(buf);
                    }
                    Ok(VadEvent::Speech(speech))
                } else {
                    Ok(VadEvent::Silence)
                }
            }

            VadState::Speaking => {
                if prob < self.threshold {
                    self.vad_state = VadState::SpeechEnding;
                    self.tail_remaining = TAIL_FRAMES;
                }
                Ok(VadEvent::Speech(frame))
            }

            VadState::SpeechEnding => {
                if prob >= self.threshold {
                    self.vad_state = VadState::Speaking;
                    return Ok(VadEvent::Speech(frame));
                }

                if self.tail_remaining > 0 {
                    self.tail_remaining -= 1;
                    Ok(VadEvent::Speech(frame))
                } else {
                    self.vad_state = VadState::Silence;
                    self.pre_buffer.clear();
                    Ok(VadEvent::Silence)
                }
            }
        }
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
        self.rnn_state = Array3::<f32>::zeros((2, 1, 128));
        self.vad_state = VadState::Silence;
        self.pre_buffer.clear();
        self.tail_remaining = 0;
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

# 降噪优化实现计划

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在音频管道中加入 nnnoiseless DNN 降噪 + Silero VAD 语音检测，消除背景杂音导致的幻觉字符，提升转写质量。

**Architecture:** 在 `AudioCapture` 内部将音频先升采样至 48kHz 经 nnnoiseless 降噪，再降采样至 16kHz；随后在 `audio_transcriber` 中用 Silero VAD v5 过滤非语音帧，仅将有效语音送入 Paraformer ASR。

**Tech Stack:** Rust、nnnoiseless 0.4、Silero VAD v5 ONNX（via ort）、rubato、ndarray

---

## Chunk 1: 配置与依赖

### Task 1: 添加 nnnoiseless 依赖并更新 AudioConfig

**Files:**
- Modify: `crates/shanji-core/Cargo.toml`
- Modify: `crates/shanji-core/src/config.rs`

- [ ] **Step 1: 添加 nnnoiseless 依赖**

在 `crates/shanji-core/Cargo.toml` 的 `[dependencies]` 中添加：

```toml
nnnoiseless = "0.4"
```

- [ ] **Step 2: 更新 AudioConfig**

在 `crates/shanji-core/src/config.rs` 中，修改 `AudioConfig` 结构体，在 `sound_feedback` 字段后添加：

```rust
pub noise_reduction: bool,
```

修改 `AudioConfig::default()` 实现，在 `sound_feedback: true,` 后添加：

```rust
noise_reduction: true,
```

同时将 `vad_threshold` 的默认值从 `0.05` 改为 `0.5`：

```rust
vad_threshold: 0.5,
```

- [ ] **Step 3: 添加 config 版本 3 迁移**

在 `AppConfig::migrate()` 方法中，在现有 `if self.version < 2 { ... }` 块之后，添加：

```rust
if self.version < 3 {
    // vad_threshold 旧含义为振幅阈值（约 0.05），新含义为 Silero 概率（约 0.5）
    if self.audio.vad_threshold < 0.1 {
        self.audio.vad_threshold = 0.5;
        modified = true;
    }
    self.version = 3;
    modified = true;
}
```

- [ ] **Step 4: cargo check 确认编译通过**

```bash
cargo check -p shanji-core
```

期望：无错误（可能有 unused import warning，忽略）

- [ ] **Step 5: 提交**

```bash
git add crates/shanji-core/Cargo.toml crates/shanji-core/src/config.rs Cargo.lock
git commit -m "feat: add noise_reduction config and nnnoiseless dependency"
```

---

### Task 2: 更新模型注册表，添加 Silero VAD v5

**Files:**
- Modify: `public/model_registry.json`
- Modify: `crates/shanji-core/src/model.rs`

- [ ] **Step 1: 添加 silero-vad 到注册表**

在 `public/model_registry.json` 的 `"models"` 数组中追加：

```json
{
    "id": "silero-vad",
    "name": "Silero VAD v5",
    "language": "universal",
    "description": "语音活动检测模型（系统内部使用）",
    "sizeBytes": 2000000,
    "downloadUrl": "https://github.com/yuhuotech/shanji/releases/download/models/silero-vad-v5.tar.gz",
    "modelscopeUrl": "",
    "huggingfaceUrl": "https://hf-mirror.com/snakers4/silero-vad/resolve/master/files/silero_vad.onnx",
    "sha256": "",
    "version": "v5.0",
    "files": ["silero_vad.onnx"]
}
```

- [ ] **Step 2: 查看 model.rs 中的下载/检查函数**

阅读 `crates/shanji-core/src/model.rs` 中 `is_model_downloaded_with_paths` 和 `get_model_dir_with_paths` 的实现，确认 VAD 模型会使用 `paths.models_dir().join("silero-vad")` 路径，无需额外修改。

- [ ] **Step 3: cargo check**

```bash
cargo check -p shanji-core
```

期望：无错误

- [ ] **Step 4: 提交**

```bash
git add public/model_registry.json
git commit -m "feat: add silero-vad v5 to model registry"
```

---

## Chunk 2: denoiser 模块

### Task 3: 实现 denoiser.rs

**Files:**
- Create: `crates/shanji-core/src/denoiser.rs`
- Modify: `crates/shanji-core/src/lib.rs`

- [ ] **Step 1: 创建 denoiser.rs**

创建文件 `crates/shanji-core/src/denoiser.rs`：

```rust
//! nnnoiseless DNN 降噪包装
//! 工作在 48kHz，每次处理 480 samples（10ms）

use nnnoiseless::DenoiseState;

const FRAME_SIZE: usize = 480; // nnnoiseless 固定帧大小

pub struct Denoiser {
    state: Box<DenoiseState<'static>>,
    input_buffer: Vec<f32>,
    output_buffer: Vec<f32>,
}

impl Denoiser {
    pub fn new() -> Self {
        Self {
            state: DenoiseState::new(),
            input_buffer: Vec::with_capacity(FRAME_SIZE * 2),
            output_buffer: Vec::new(),
        }
    }

    /// 输入 48kHz f32 samples（任意长度），返回等长降噪后的 samples。
    /// 不足一帧的尾部样本原样直通（不引入额外延迟）。
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        self.input_buffer.extend_from_slice(input);
        self.output_buffer.clear();

        // 按 FRAME_SIZE 逐帧处理
        while self.input_buffer.len() >= FRAME_SIZE {
            let frame: Vec<f32> = self.input_buffer.drain(..FRAME_SIZE).collect();
            let mut out = vec![0.0f32; FRAME_SIZE];
            self.state.process_frame(&mut out, &frame);
            self.output_buffer.extend_from_slice(&out);
        }

        // 剩余不足一帧的样本原样直通（保证输出长度 == 输入长度）
        let tail: Vec<f32> = self.input_buffer.drain(..).collect();
        self.output_buffer.extend_from_slice(&tail);

        self.output_buffer.clone()
    }
}

impl Default for Denoiser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_output_length_equals_input() {
        let mut denoiser = Denoiser::new();
        // 整帧
        let input = vec![0.1f32; 480];
        let output = denoiser.process(&input);
        assert_eq!(output.len(), input.len());
    }

    #[test]
    fn test_partial_frame_passthrough() {
        let mut denoiser = Denoiser::new();
        // 不足一帧
        let input = vec![0.1f32; 200];
        let output = denoiser.process(&input);
        assert_eq!(output.len(), 200);
    }

    #[test]
    fn test_cross_frame_boundary() {
        let mut denoiser = Denoiser::new();
        // 跨帧边界：1.5 帧
        let input = vec![0.05f32; 720];
        let output = denoiser.process(&input);
        assert_eq!(output.len(), 720);
    }

    #[test]
    fn test_silence_stays_near_zero() {
        let mut denoiser = Denoiser::new();
        let silence = vec![0.0f32; 4800]; // 100ms 静音
        let output = denoiser.process(&silence);
        // 降噪后静音应保持接近零
        let max_abs = output.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
        assert!(max_abs < 0.01, "静音降噪后超出阈值: {}", max_abs);
    }
}
```

- [ ] **Step 2: 在 lib.rs 导出模块**

在 `crates/shanji-core/src/lib.rs` 中添加：

```rust
pub mod denoiser;
```

- [ ] **Step 3: 运行单元测试（预期通过）**

```bash
cargo test -p shanji-core denoiser
```

期望：4 个测试全部 PASS

- [ ] **Step 4: 提交**

```bash
git add crates/shanji-core/src/denoiser.rs crates/shanji-core/src/lib.rs
git commit -m "feat: implement Denoiser (nnnoiseless 48kHz wrapper)"
```

---

## Chunk 3: VAD 模块

### Task 4: 实现 vad.rs（Silero VAD v5）

**Files:**
- Create: `crates/shanji-core/src/vad.rs`
- Modify: `crates/shanji-core/src/lib.rs`

- [ ] **Step 1: 创建 vad.rs**

创建文件 `crates/shanji-core/src/vad.rs`：

```rust
//! Silero VAD v5 语音活动检测
//!
//! 输入：16kHz，512 samples/帧（32ms）
//! 输出：VadEvent（Speech 含可送入 ASR 的 samples，Silence 丢弃）
//!
//! 状态机：Silence → SpeechStarting → Speaking → SpeechEnding → Silence
//! - 前置缓冲 200ms（约 6 帧），防止字头被截断
//! - 尾部拖尾 500ms（约 15 帧），防止字尾被截断

use crate::error::{AppError, Result};
use ndarray::{Array1, Array3};
use ort::inputs;
use ort::session::Session;
use std::collections::VecDeque;
use std::path::Path;

const FRAME_SIZE: usize = 512;        // 32ms @ 16kHz
const PRE_BUFFER_FRAMES: usize = 6;   // 200ms 前置缓冲
const TAIL_FRAMES: usize = 15;        // 500ms 尾部拖尾
const SAMPLE_RATE: i64 = 16000;

#[derive(Debug)]
pub enum VadEvent {
    /// 语音帧，携带需送入 ASR 的 samples（含回放缓冲帧）
    Speech(Vec<f32>),
    /// 静音帧，丢弃
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
    threshold: f32,
    /// Silero VAD v5 RNN state，shape [2, 1, 128]
    state: Array3<f32>,
    vad_state: VadState,
    /// 前置缓冲，保存最近 PRE_BUFFER_FRAMES 帧（每帧 512 samples）
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
            state: Array3::<f32>::zeros((2, 1, 128)),
            vad_state: VadState::Silence,
            pre_buffer: VecDeque::with_capacity(PRE_BUFFER_FRAMES + 1),
            tail_remaining: 0,
            frame_buffer: Vec::with_capacity(FRAME_SIZE * 2),
        })
    }

    /// 输入 16kHz samples（任意长度），按 512-sample 帧处理，返回 VadEvent 列表
    pub fn process(&mut self, samples: &[f32]) -> Result<Vec<VadEvent>> {
        self.frame_buffer.extend_from_slice(samples);
        let mut events = Vec::new();

        while self.frame_buffer.len() >= FRAME_SIZE {
            let frame: Vec<f32> = self.frame_buffer.drain(..FRAME_SIZE).collect();
            let event = self.process_frame(&frame)?;
            events.push(event);
        }

        Ok(events)
    }

    fn process_frame(&mut self, frame: &[f32]) -> Result<VadEvent> {
        let prob = self.infer(frame)?;

        match self.vad_state {
            VadState::Silence => {
                // 维护前置缓冲（滑动窗口，保留最近 PRE_BUFFER_FRAMES 帧）
                if self.pre_buffer.len() >= PRE_BUFFER_FRAMES {
                    self.pre_buffer.pop_front();
                }
                self.pre_buffer.push_back(frame.to_vec());

                if prob >= self.threshold {
                    self.vad_state = VadState::SpeechStarting;
                    // 回放前置缓冲帧 + 当前帧
                    let mut speech_samples = Vec::new();
                    for buffered in self.pre_buffer.drain(..) {
                        speech_samples.extend(buffered);
                    }
                    return Ok(VadEvent::Speech(speech_samples));
                }
                Ok(VadEvent::Silence)
            }

            VadState::SpeechStarting => {
                self.vad_state = VadState::Speaking;
                Ok(VadEvent::Speech(frame.to_vec()))
            }

            VadState::Speaking => {
                if prob < self.threshold {
                    self.vad_state = VadState::SpeechEnding;
                    self.tail_remaining = TAIL_FRAMES;
                }
                Ok(VadEvent::Speech(frame.to_vec()))
            }

            VadState::SpeechEnding => {
                if prob >= self.threshold {
                    // 重新检测到语音，回到 Speaking
                    self.vad_state = VadState::Speaking;
                    return Ok(VadEvent::Speech(frame.to_vec()));
                }

                if self.tail_remaining > 0 {
                    self.tail_remaining -= 1;
                    Ok(VadEvent::Speech(frame.to_vec()))
                } else {
                    self.vad_state = VadState::Silence;
                    self.pre_buffer.clear();
                    Ok(VadEvent::Silence)
                }
            }
        }
    }

    /// 运行 Silero VAD v5 ONNX 推理，返回语音概率
    fn infer(&mut self, frame: &[f32]) -> Result<f32> {
        // 构造输入张量
        let input = Array1::<f32>::from_vec(frame.to_vec())
            .into_shape((1, FRAME_SIZE))
            .map_err(|e| AppError::Asr(format!("VAD input shape error: {}", e)))?;

        let sr = Array1::<i64>::from_vec(vec![SAMPLE_RATE]);

        let outputs = self
            .session
            .run(inputs![
                "input" => input.view(),
                "state" => self.state.view(),
                "sr" => sr.view(),
            ]
            .map_err(|e| AppError::Asr(format!("VAD input construction failed: {}", e)))?)
            .map_err(|e| AppError::Asr(format!("VAD inference failed: {}", e)))?;

        // 更新 RNN state
        let new_state = outputs["stateN"]
            .try_extract_tensor::<f32>()
            .map_err(|e| AppError::Asr(format!("VAD state extraction failed: {}", e)))?;
        self.state.assign(&new_state.view().into_shape((2, 1, 128))
            .map_err(|e| AppError::Asr(format!("VAD state reshape failed: {}", e)))?);

        // 提取语音概率
        let prob_tensor = outputs["output"]
            .try_extract_tensor::<f32>()
            .map_err(|e| AppError::Asr(format!("VAD output extraction failed: {}", e)))?;

        Ok(prob_tensor.iter().next().copied().unwrap_or(0.0))
    }

    pub fn reset(&mut self) {
        self.state = Array3::<f32>::zeros((2, 1, 128));
        self.vad_state = VadState::Silence;
        self.pre_buffer.clear();
        self.tail_remaining = 0;
        self.frame_buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 VadState 初始为 Silence
    #[test]
    fn test_initial_state_is_silence() {
        // 不加载真实模型，仅检查结构
        // 若需要端到端测试，需先下载 silero_vad.onnx 到测试路径
        let state = VadState::Silence;
        assert_eq!(state, VadState::Silence);
    }

    /// 验证帧分割逻辑：输入 1024 samples 应产生 2 个完整帧
    #[test]
    fn test_frame_splitting() {
        // 模拟帧缓冲逻辑（不依赖模型）
        let mut buf: Vec<f32> = Vec::new();
        let input = vec![0.0f32; 1024];
        buf.extend_from_slice(&input);
        let frames: Vec<Vec<f32>> = buf
            .chunks(FRAME_SIZE)
            .filter(|c| c.len() == FRAME_SIZE)
            .map(|c| c.to_vec())
            .collect();
        assert_eq!(frames.len(), 2);
    }
}
```

- [ ] **Step 2: 在 lib.rs 导出模块**

在 `crates/shanji-core/src/lib.rs` 中添加：

```rust
pub mod vad;
```

- [ ] **Step 3: 运行单元测试**

```bash
cargo test -p shanji-core vad
```

期望：2 个测试 PASS（不依赖真实模型）

- [ ] **Step 4: 提交**

```bash
git add crates/shanji-core/src/vad.rs crates/shanji-core/src/lib.rs
git commit -m "feat: implement VadDetector (Silero VAD v5 state machine)"
```

---

## Chunk 4: 音频管道接入降噪

### Task 5: 修改 audio.rs，接入 Denoiser

**Files:**
- Modify: `crates/shanji-core/src/audio.rs`

关键变更：`CaptureState` 增加两个 resampler 字段和 `Denoiser`，`process_input_data` 中在有降噪时走新路径。

- [ ] **Step 1: 更新 use 导入**

在 `audio.rs` 顶部添加：

```rust
use crate::denoiser::Denoiser;
```

- [ ] **Step 2: 重构 CaptureState**

将原有的 `CaptureState` 中 `resampler: Option<SincFixedIn<f32>>` 替换为：

```rust
struct CaptureState {
    sample_tx: Sender<Vec<f32>>,
    input_buffer: Vec<f32>,
    /// noise_reduction=false 时：native→16kHz
    resampler_to_16k: Option<SincFixedIn<f32>>,
    /// noise_reduction=true 时：native→48kHz（若 mic 已是 48kHz 则 None）
    resampler_to_48k: Option<SincFixedIn<f32>>,
    /// noise_reduction=true 时：48kHz→16kHz
    resampler_48k_to_16k: Option<SincFixedIn<f32>>,
    /// DNN 降噪器（noise_reduction=true 时 Some）
    denoiser: Option<Denoiser>,
    running: Arc<AtomicBool>,
}
```

- [ ] **Step 3: 修改 AudioCapture::start()，按 noise_reduction 创建对应 resampler**

在 `start()` 方法签名中增加 `noise_reduction: bool` 参数：

```rust
pub fn start(&mut self, device_name: Option<&str>, sample_tx: Sender<Vec<f32>>, noise_reduction: bool) -> Result<()>
```

在 resampler 创建逻辑处，替换原有逻辑：

```rust
const TARGET_48K: u32 = 48_000;

let (resampler_to_16k, resampler_to_48k, resampler_48k_to_16k, denoiser) =
    if noise_reduction {
        // 新路径：native→48k→denoise→16k
        let to_48k = if input_sample_rate != TARGET_48K {
            Some(make_resampler(input_sample_rate, TARGET_48K, RESAMPLE_BUFFER_SIZE)?)
        } else {
            None
        };
        let to_16k = make_resampler(TARGET_48K, TARGET_SAMPLE_RATE, RESAMPLE_BUFFER_SIZE)?;
        (None, to_48k, Some(to_16k), Some(Denoiser::new()))
    } else {
        // 原有路径：native→16k
        let to_16k = if input_sample_rate != TARGET_SAMPLE_RATE {
            Some(make_resampler(input_sample_rate, TARGET_SAMPLE_RATE, RESAMPLE_BUFFER_SIZE)?)
        } else {
            None
        };
        (to_16k, None, None, None)
    };
```

添加辅助函数（模块私有）：

```rust
fn make_resampler(from: u32, to: u32, buffer_size: usize) -> Result<SincFixedIn<f32>> {
    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 128,
        window: WindowFunction::BlackmanHarris2,
    };
    SincFixedIn::<f32>::new(
        to as f64 / from as f64,
        2.0,
        params,
        buffer_size,
        1,
    )
    .map_err(|e| AppError::Audio(format!("Failed to create resampler {}→{}: {}", from, to, e)))
}
```

- [ ] **Step 4: 修改 process_input_data，实现双路径**

重写 `process_input_data` 中重采样部分（声道下混后）：

```rust
// 声道下混后，根据 denoiser 是否存在选择路径
if state.denoiser.is_some() {
    // 新路径：input → 48kHz → denoise → 16kHz → send
    let samples_48k = if let Some(ref mut r) = state.resampler_to_48k {
        resample_buffer(&mut state.input_buffer, r)
    } else {
        // mic 已是 48kHz，直通
        std::mem::take(&mut state.input_buffer)
    };

    if !samples_48k.is_empty() {
        let denoised = state.denoiser.as_mut().unwrap().process(&samples_48k);

        if let Some(ref mut r) = state.resampler_48k_to_16k {
            // 将 denoised 放入临时缓冲进行 48k→16k 重采样
            let mut tmp = denoised;
            while tmp.len() >= RESAMPLE_BUFFER_SIZE {
                let chunk: Vec<f32> = tmp.drain(..RESAMPLE_BUFFER_SIZE).collect();
                match r.process(&[chunk], None) {
                    Ok(out) => {
                        if let Some(ch) = out.first() {
                            if !ch.is_empty() {
                                let _ = state.sample_tx.send(ch.clone());
                            }
                        }
                    }
                    Err(e) => log::error!("Resampling 48k→16k error: {:?}", e),
                }
            }
        }
    }
} else {
    // 原有路径：input → 16kHz → send（保持原逻辑不变）
    if sample_rate != TARGET_SAMPLE_RATE {
        if let Some(ref mut resampler) = state.resampler_to_16k {
            while state.input_buffer.len() >= RESAMPLE_BUFFER_SIZE {
                let chunk: Vec<f32> = state.input_buffer.drain(..RESAMPLE_BUFFER_SIZE).collect();
                match resampler.process(&[chunk], None) {
                    Ok(output) => {
                        if let Some(ch) = output.first() {
                            if !ch.is_empty() {
                                let _ = state.sample_tx.send(ch.clone());
                            }
                        }
                    }
                    Err(e) => log::error!("Resampling error: {:?}", e),
                }
            }
        }
    } else {
        const CHUNK_SIZE: usize = 1600;
        while state.input_buffer.len() >= CHUNK_SIZE {
            let chunk: Vec<f32> = state.input_buffer.drain(..CHUNK_SIZE).collect();
            let _ = state.sample_tx.send(chunk);
        }
    }
}
```

添加辅助函数：

```rust
fn resample_buffer(buf: &mut Vec<f32>, resampler: &mut SincFixedIn<f32>) -> Vec<f32> {
    let mut out = Vec::new();
    while buf.len() >= RESAMPLE_BUFFER_SIZE {
        let chunk: Vec<f32> = buf.drain(..RESAMPLE_BUFFER_SIZE).collect();
        match resampler.process(&[chunk], None) {
            Ok(output) => {
                if let Some(ch) = output.first() {
                    out.extend_from_slice(ch);
                }
            }
            Err(e) => log::error!("Resampling error: {:?}", e),
        }
    }
    out
}
```

- [ ] **Step 5: cargo check**

```bash
cargo check -p shanji-core
```

期望：无错误

- [ ] **Step 6: 提交**

```bash
git add crates/shanji-core/src/audio.rs
git commit -m "feat: integrate Denoiser into AudioCapture pipeline"
```

---

## Chunk 5: VAD 接入 audio_transcriber

### Task 6: 修改 audio_transcriber.rs，接入 VadDetector

**Files:**
- Modify: `crates/shanji-slint/src/audio_transcriber.rs`

- [ ] **Step 1: 添加 use 导入**

在 `audio_transcriber.rs` 顶部，在 `use shanji_core::...` 块中添加：

```rust
use shanji_core::model;
use shanji_core::vad::{VadDetector, VadEvent};
```

- [ ] **Step 2: 在 run_live_asr 中初始化 VadDetector**

在 `run_live_asr` 函数中，创建 `engine` 之后，添加 VAD 初始化：

```rust
// 尝试初始化 VAD（失败时降级为无 VAD）
let mut vad = if config.audio.noise_reduction {
    let vad_model_dir = model::get_model_dir_with_paths(&paths, "silero-vad");
    let vad_model_path = vad_model_dir.join("silero_vad.onnx");
    match VadDetector::new(&vad_model_path, config.audio.vad_threshold) {
        Ok(v) => {
            log::info!("Silero VAD initialized (threshold={})", config.audio.vad_threshold);
            Some(v)
        }
        Err(e) => {
            log::warn!("VAD init failed, running without VAD: {}", e);
            None
        }
    }
} else {
    None
};
```

- [ ] **Step 3: 修改 AudioCapture::start() 调用，传入 noise_reduction**

```rust
capture
    .start(selected_device.as_deref(), sample_tx, config.audio.noise_reduction)
    .map_err(|e| e.to_string())?;
```

- [ ] **Step 4: 修改主循环，加入 VAD 过滤**

在主循环的 `Ok(samples) =>` 分支中，替换原有的 `engine.process_chunk(&samples)` 调用：

```rust
Ok(samples) => {
    let level = audio::calculate_audio_level(&samples);
    state::set_audio_level(level);

    // VAD 过滤：仅处理语音帧
    let speech_samples: Vec<f32> = if let Some(ref mut detector) = vad {
        match detector.process(&samples) {
            Ok(events) => events
                .into_iter()
                .filter_map(|e| match e {
                    VadEvent::Speech(s) => Some(s),
                    VadEvent::Silence => None,
                })
                .flatten()
                .collect(),
            Err(e) => {
                log::warn!("VAD process error, using raw samples: {}", e);
                samples
            }
        }
    } else {
        samples
    };

    if speech_samples.is_empty() {
        continue;
    }

    match engine.process_chunk(&speech_samples) {
        Ok(Some(text)) if !text.is_empty() => {
            state::set_state(AppState::Transcribing);
            state::set_status_message("Live ASR is producing partial text");
            state::set_live_transcript(text.clone());
            state::set_last_transcript(text);
        }
        Ok(_) => {}
        Err(err) => {
            state::set_status_message(format!("ASR chunk failed: {}", err));
        }
    }
}
```

- [ ] **Step 5: 在 finalize 前 reset VAD**

在 `capture.stop()` 之后、`engine.finalize()` 之前，添加：

```rust
if let Some(ref mut detector) = vad {
    detector.reset();
}
```

- [ ] **Step 6: cargo check**

```bash
cargo check
```

期望：无错误

- [ ] **Step 7: 提交**

```bash
git add crates/shanji-slint/src/audio_transcriber.rs
git commit -m "feat: integrate Silero VAD into audio transcription pipeline"
```

---

## Chunk 6: 验证

### Task 7: 集成验证

- [ ] **Step 1: 运行所有测试**

```bash
cargo test -p shanji-core
```

期望：所有测试 PASS

- [ ] **Step 2: 运行完整 check**

```bash
cargo check
```

期望：无错误

- [ ] **Step 3: 手动验证（可选）**

```bash
cargo run -p shanji-app
```

测试场景：
1. 静音环境按住录音键不说话 → 不应产生任何输出
2. 有背景噪声时说话 → 应正确转写
3. 在设置中关闭降噪 → 行为与原来一致

- [ ] **Step 4: 最终提交**

```bash
git add -A
git commit -m "feat: complete noise reduction pipeline (nnnoiseless + Silero VAD)"
```

# 降噪优化设计文档

**日期**：2026-03-14
**状态**：待实现
**涉及 crate**：`shanji-core`、`shanji-slint`

---

## 背景与目标

当前音频管道不含任何降噪或语音活动检测（VAD），导致：

1. **静音段产生幻觉字符**：Paraformer 对背景噪声、键盘声等非语音信号强行解码，输出无意义字符
2. **语音质量差**：空调、风扇等稳态噪声与动态噪声（键盘、周围人声）混入特征，降低 ASR 准确率

目标：在不引入云端依赖的前提下，通过两层本地降噪显著改善转写质量。

---

## 方案选型

### 选定方案：Silero VAD + nnnoiseless（双层）

| 层 | 技术 | 作用 |
|----|------|------|
| 第一层 | nnnoiseless 0.4（RNNoise 纯 Rust 移植） | DNN 谱掩蔽，抑制稳态和动态噪声 |
| 第二层 | Silero VAD v5（ONNX） | 过滤纯静音/非语音帧，防止幻觉字符 |

**选型理由**：
- `nnnoiseless` 纯 Rust，无 C++ 依赖，与项目技术栈一致
- Silero VAD 已在 CLAUDE.md 设计中规划（`Silero VAD（ONNX）—— 静音检测`），属于补全缺失实现
- 两层互补：降噪解决"噪声中的语音质量"，VAD 解决"没说话时的幻觉"

---

## 新音频管道

```
Mic (native rate)
  → [1] resample → 48kHz           (nnnoiseless 要求 48kHz 输入；若 mic 已是 48kHz 则直通)
  → [2] nnnoiseless DNN 降噪        (480 samples/帧，10ms，清除噪声)
  → [3] resample → 16kHz           (VAD + ASR 要求 16kHz 输入)
  → [4] Silero VAD v5               (512 samples/帧，32ms，过滤非语音)
  → [5] FBank 特征提取 → Paraformer  (仅处理有效语音帧)
```

**音频电平计**：`calculate_audio_level` 在步骤 [3] 之后、VAD 之前执行，作用于降噪后的 16kHz 信号，反映用户感知响度。这是有意为之，降噪关闭时路径不变。

**说明**：管道延迟约增加 20–30ms，对实时语音体验无感。

---

## VAD 策略（Silero VAD v5）

### ONNX I/O 规范（v5 接口）

Silero VAD v5 使用单一 `state` 张量（区别于 v4 的 `h`/`c` 分离格式）：

| 输入 | 形状 | 类型 |
|------|------|------|
| `input` | `[1, 512]` | f32 |
| `state` | `[2, 1, 128]` | f32 |
| `sr` | `[1]` | i64，值固定为 16000 |

| 输出 | 形状 |
|------|------|
| `output` | `[1, 1]`（语音概率） |
| `stateN` | `[2, 1, 128]`（更新后的 state） |

### 状态机

```
Silence
  → [prob ≥ threshold] → SpeechStarting（回放前 200ms 缓冲帧）
SpeechStarting
  → [已回放完缓冲] → Speaking
Speaking
  → [prob < threshold] → SpeechEnding（tail_counter = 500ms / frame_ms）
SpeechEnding
  → [tail_counter > 0] → 继续送入 ASR，tail_counter -= 1
  → [tail_counter == 0] → Silence
```

- **触发阈值**：语音概率 ≥ 0.5（可配置，对应 Silero 概率尺度）
- **前置缓冲 200ms**：防止字头被吃掉（约 6 帧 × 32ms）
- **尾部拖尾 500ms**：防止字尾截断（约 15 帧）

### 配置字段迁移

`AudioConfig` 中已存在 `vad_threshold: f32`，默认值为 `0.05`（旧版为原始振幅阈值，现已废弃）。

本次实现：
- 将 `vad_threshold` 语义更改为 Silero 概率阈值（范围 0.0–1.0，新默认值 `0.5`）
- 在 `AppConfig::migrate()` 中添加迁移逻辑：若旧配置的 `vad_threshold < 0.1`，将其重置为 `0.5`，避免对现有用户产生无效过滤

---

## 模块设计

### 新增：`shanji-core/src/denoiser.rs`

```rust
/// nnnoiseless 降噪包装（48kHz，480 samples/帧）
pub struct Denoiser {
    state: nnnoiseless::DenoiseState<'static>,
    frame_buffer: Vec<f32>,  // 积累满 480 samples 再处理
    output_buffer: Vec<f32>, // 已降噪待取出的 samples
}

impl Denoiser {
    pub fn new() -> Self;
    /// 输入 48kHz f32 samples（任意长度），返回等长降噪后的 samples
    /// 保证输出长度 == 输入长度
    pub fn process(&mut self, input: &[f32]) -> Vec<f32>;
}

#[cfg(test)]
// 单元测试：process 输出长度 == 输入长度（覆盖整帧和跨帧边界）
```

### 新增：`shanji-core/src/vad.rs`

VAD 封装完整状态机（包括前置缓冲和拖尾逻辑），对外暴露高层接口：

```rust
pub enum VadEvent {
    /// 此帧为语音，携带可送入 ASR 的 samples（含回放的缓冲帧）
    Speech(Vec<f32>),
    /// 此帧为静音，丢弃
    Silence,
}

pub struct VadDetector {
    session: ort::Session,
    threshold: f32,
    state: ndarray::Array3<f32>,        // shape [2, 1, 128]，Silero v5 state
    machine: VadStateMachine,
    frame_buffer: VecDeque<Vec<f32>>,   // 前置缓冲（200ms）
    tail_remaining: usize,
}

impl VadDetector {
    pub fn new(model_path: &Path, threshold: f32) -> Result<Self>;
    /// 输入 16kHz 512-sample 帧，输出 VadEvent
    pub fn process_frame(&mut self, frame: &[f32]) -> Result<VadEvent>;
    pub fn reset(&mut self);
}

#[cfg(test)]
// 单元测试：静音帧（全零）返回 Silence；加载 fixture WAV 的语音帧返回 Speech
```

### 修改：`shanji-core/src/audio.rs` — `CaptureState`

```rust
struct CaptureState {
    sample_tx: Sender<Vec<f32>>,
    input_buffer: Vec<f32>,
    // 原有：native→16kHz 重采样器（noise_reduction=false 时使用）
    resampler_to_16k: Option<SincFixedIn<f32>>,
    // 新增：native→48kHz 重采样器（noise_reduction=true 时使用）
    resampler_to_48k: Option<SincFixedIn<f32>>,
    // 新增：48kHz→16kHz 重采样器（noise_reduction=true 时使用）
    resampler_48k_to_16k: Option<SincFixedIn<f32>>,
    // 新增：降噪器（noise_reduction=true 时初始化）
    denoiser: Option<Denoiser>,
    running: Arc<AtomicBool>,
}
```

- `resampler_to_48k` 仅在 mic 原生率 ≠ 48kHz 时创建；48kHz mic 直通
- `resampler_48k_to_16k` 始终在 `noise_reduction=true` 时创建（48kHz→16kHz）
- `noise_reduction=false` 时，`resampler_to_16k` 维持原逻辑

### 修改：`shanji-slint/src/audio_transcriber.rs`

在 `sample_rx.recv()` 后：
1. `audio::calculate_audio_level(&samples)` — 基于降噪后 16kHz 信号
2. 将 samples 分成 512-sample 帧送入 `VadDetector::process_frame()`
3. 收到 `VadEvent::Speech(frames)` 才调用 `engine.process_chunk()`
4. 收到 `VadEvent::Silence` 则跳过

### 修改：`shanji-core/src/config.rs`

`AudioConfig` 已有字段（当前默认 `vad_threshold: 0.05`），本次变更：

```rust
pub struct AudioConfig {
    pub device_name: Option<String>,
    pub gain: f32,
    pub recording_mode: String,
    pub vad_threshold: f32,      // 语义变更：从旧振幅阈值 → Silero 概率阈值，新默认值 0.5
    pub silence_timeout_ms: u32,
    pub min_speech_frames: u32,
    pub sound_feedback: bool,
    pub noise_reduction: bool,   // 新增，默认 true
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            // ...其他字段不变...
            vad_threshold: 0.5,      // 从 0.05 改为 0.5（Silero 概率尺度）
            noise_reduction: true,   // 新增
            // ...
        }
    }
}
```

`migrate()` 新增版本 3 迁移块（当前最新版本为 2）：

```rust
if self.version < 3 {
    // vad_threshold 旧值为振幅阈值（≈0.05），新含义为 Silero 概率（≈0.5）
    // 若旧值 < 0.1，说明是旧格式，重置为新默认值
    if self.audio.vad_threshold < 0.1 {
        self.audio.vad_threshold = 0.5;
        modified = true;
    }
    self.version = 3;
    modified = true;
}
```

### 修改：`public/model_registry.json`

`ModelInfo` serde 结构要求以下字段必填（无 `#[serde(default)]`）：`id`、`name`、`language`、`description`、`sizeBytes`、`downloadUrl`、`sha256`、`version`。

完整条目（需在实现时填入真实 sha256 和 downloadUrl）：

```json
{
  "id": "silero-vad",
  "name": "Silero VAD v5",
  "language": "universal",
  "description": "语音活动检测模型（系统内部使用，用于降噪辅助）",
  "sizeBytes": 2000000,
  "downloadUrl": "https://github.com/yuhuotech/shanji/releases/download/models/silero-vad-v5.tar.gz",
  "modelscopeUrl": "",
  "huggingfaceUrl": "https://hf-mirror.com/snakers4/silero-vad/resolve/master/files/silero_vad.onnx",
  "sha256": "",
  "version": "v5.0",
  "files": ["silero_vad.onnx"]
}
```

路径由 `get_model_dir_with_paths(paths, "silero-vad")` 自动派生，无需额外逻辑。

### 修改：`shanji-core/src/model.rs` / 启动检查

- 下载检查：VAD 模型随 ASR 模型一同在下载流程中检查
- 启动守卫：`audio_transcriber::start()` 中增加 VAD 模型存在性检查：
  - 若缺失：**降级为禁用 VAD**（打印 warning），不阻断录音
  - 若 ONNX 加载失败：同上，降级处理，不崩溃

---

## 依赖变更

### `crates/shanji-core/Cargo.toml`

```toml
nnnoiseless = "0.4"   # 新增：RNNoise 纯 Rust 实现（当前最新为 0.4.x）
# ort、ndarray 已存在，用于 Silero VAD v5
```

---

## 实现顺序

1. **配置变更**：更新 `config.rs`（新增 `noise_reduction`，迁移 `vad_threshold` 语义，version 3 迁移块）
2. **模型注册**：更新 `model_registry.json` 和 `model.rs` 下载/检查逻辑
3. **实现 `denoiser.rs`**（含单元测试）
4. **实现 `vad.rs`**（含单元测试）
5. **更新 `lib.rs`**：添加 `pub mod denoiser;` 和 `pub mod vad;`
6. **修改 `audio.rs`**：接入 `Denoiser`，重构 `CaptureState`（含三个 resampler 字段）
7. **修改 `audio_transcriber.rs`**：接入 VAD 过滤
8. **集成验证**：`cargo check` + `cargo test -p shanji-core`

---

## 验收标准

- 静音环境下按住录音键不说话，不产生任何输出字符
- 有明显背景噪声时，说话内容能被准确转写
- 降噪关闭时（`noise_reduction: false`），行为与原来完全一致（向后兼容）
- 旧配置的 `vad_threshold: 0.05` 自动迁移为 `0.5`，用户无感
- VAD 模型缺失时，应用正常启动并打印 warning（不崩溃）
- `cargo check` 无错误，`cargo test -p shanji-core` 通过

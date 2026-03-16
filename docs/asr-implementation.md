# ASR 推理引擎实现文档

本文档描述闪记（Shanji）ASR 推理引擎的实现细节。

## 架构概览

```
音频采集 (cpal)
    ↓
FBank 特征提取 (80-dim, 16kHz)
    ↓
Paraformer Encoder (ONNX Runtime)
    ↓
Paraformer Decoder / CTC (ONNX Runtime)
    ↓
Tokenizer (BPE/Char)
    ↓
文本输出
```

## 核心模块

### 1. ASR 引擎 (`crates/shanji-core/src/asr.rs`)

```rust
pub struct AsrEngine {
    tokenizer: Option<Tokenizer>,
    feature_extractor: FBankExtractor,
    config: AsrConfig,
    streaming: Option<StreamingParaformer>,
    sample_buffer: Vec<f32>,
    partial_result: String,
}
```

主要功能：
- `load_model()`: 加载 ONNX 模型和词汇表
- `process_chunk()`: 处理音频块（流式识别）
- `finalize()`: 结束识别并返回结果
- `recognize()`: 非流式完整识别

### 2. Paraformer 推理 (`crates/shanji-core/src/asr/paraformer.rs`)

包含三个核心结构：

#### ParaformerEncoder
- 处理 FBank 特征
- 支持流式缓存（cache）
- 输入: `[batch, frames, 80]`
- 输出: `[batch, frames, hidden_dim]`

#### ParaformerDecoder
- CTC 解码
- 输入: encoder 输出
- 输出: token 概率分布

#### StreamingParaformer
- 管理特征缓冲区
- 控制 chunk 大小（默认 67 帧 ≈ 1 秒）
- 协调 encoder/decoder

### 3. 特征提取 (`crates/shanji-core/src/asr/feature.rs`)

FBank (Filter Bank) 特征：
- 采样率: 16kHz
- 帧长: 25ms
- 帧移: 10ms
- 维度: 80

### 4. Tokenizer (`crates/shanji-core/src/asr/tokenizer.rs`)

支持：
- `vocab.txt` 格式（每行一个 token）
- `vocab.json` 格式（JSON 映射）
- 字符级 fallback

## ONNX Runtime 适配

闪记使用 `ort` 2.0.0-rc.5，主要适配点：

### 输入创建
```rust
let tensor = Tensor::from_array(([batch, frames, mels], data.into_boxed_slice()))?;
let outputs = session.run(ort::inputs! {
    "speech" => tensor,
})?;
```

### 输出提取
```rust
let (shape, data) = outputs["encoder_out"].try_extract_tensor::<f32>()?;
let array = Array3::from_shape_vec(
    (shape[0] as usize, shape[1] as usize, shape[2] as usize),
    data.to_vec()
)?;
```

## 模型文件格式

### 目录结构
```
models/
└── paraformer-zh/
    ├── encoder.onnx      # 必需
    ├── decoder.onnx      # 可选（有些模型合并了 encoder/decoder）
    ├── vocab.txt         # 词汇表
    └── config.yaml       # 配置
```

### 词汇表格式
```
<pad>
<unk>
<s>
</s>
你
好
世
界
...
```

## 模型获取方式

### 方式 1: 使用 FunASR 导出脚本

```bash
cd scripts
pip install funasr modelscope torch onnx
python export_funasr_model.py \
    --model iic/speech_paraformer-large_asr_nat-zh-cn-16k-common-vocab8404-pytorch \
    --output ./models/paraformer-zh
```

### 方式 2: 直接下载 ONNX 模型

```bash
cd scripts
pip install requests tqdm
python download_models.py --model paraformer-zh
```

### 方式 3: 手动放置

1. 从 ModelScope/HuggingFace 下载 ONNX 模型
2. 放入应用数据目录：
   - macOS: `~/Library/Application Support/shanji/models/`
   - Windows: `%APPDATA%/shanji/models/`
   - Linux: `~/.local/share/shanji/models/`

## 性能优化

### 1. 流式处理
- 每 1 秒音频（16000 样本）处理一次
- 使用左上下文（6 帧）提高精度
- 缓存 encoder 状态避免重复计算

### 2. 内存管理
- 使用 `Arc<Session>` 共享模型（虽然实际实现中 Session 不实现 Clone，由 StreamingParaformer 持有）
- 特征缓冲区自动清理

### 3. 推理参数
```rust
AsrConfig {
    chunk_size: 67,      // 帧数
    left_context: 6,     // 左上下文帧数
    right_context: 0,    // 右上下文帧数（流式为 0）
}
```

## 调试技巧

### 启用详细日志
```bash
RUST_LOG=debug cargo run
```

### 测试特征提取
```rust
let features = feature_extractor.extract(&audio_samples)?;
println!("Extracted {} frames", features.len());
```

### 验证 ONNX 模型
```rust
let session = Session::builder()?.commit_from_file("encoder.onnx")?;
println!("Inputs: {:?}", session.inputs);
println!("Outputs: {:?}", session.outputs);
```

## 常见问题

### Q: 模型加载失败
A: 检查：
1. encoder.onnx 文件是否存在
2. ONNX 版本是否兼容（opset 14+）
3. 文件权限是否正确

### Q: 识别结果为空
A: 检查：
1. 音频电平是否正常（非静音）
2. 采样率是否为 16kHz
3. vocab.txt 是否匹配模型

### Q: 内存占用过高
A: 优化：
1. 减小 chunk_size
2. 使用量化模型（INT8）
3. 限制并发识别任务

## 参考链接

- [FunASR GitHub](https://github.com/alibaba-damo-academy/FunASR)
- [ort crate docs](https://docs.rs/ort/2.0.0-rc.5/ort/)
- [ONNX Runtime](https://onnxruntime.ai/)

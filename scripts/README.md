# 模型工具说明

这些脚本用于导出、下载和发布 FunASR Paraformer ONNX 模型，供闪记（Shanji）使用。

它们独立于 UI 层，可单独维护。

## 依赖安装

```bash
pip install funasr modelscope torch onnx
```

## 常见用途

### 1. 下载模型

```bash
python download_models.py
```

### 2. 导出非流式中文模型

```bash
python export_funasr_model.py \
    --model damo/speech_paraformer-large_asr_nat-zh-cn-16k-common-vocab8404-pytorch \
    --output ./models/paraformer-zh
```

### 3. 导出流式中文模型

```bash
python export_funasr_model.py \
    --model damo/speech_paraformer-large_asr_nat-zh-cn-16k-common-vocab8404-online \
    --output ./models/paraformer-zh-streaming
```

## 支持的模型

| 模型 | ModelScope ID | 说明 |
|------|--------------|------|
| Paraformer 中文 | `damo/speech_paraformer-large_asr_nat-zh-cn-16k-common-vocab8404-pytorch` | 非流式，精度高 |
| Paraformer 流式中文 | `damo/speech_paraformer-large_asr_nat-zh-cn-16k-common-vocab8404-online` | 流式，延迟低 |
| Paraformer 英文 | `damo/speech_paraformer-large_asr_nat-en-16k-common-vocab10020` | 英语识别 |

## 输出文件结构

```text
models/paraformer-zh/
├── encoder.onnx
├── decoder.onnx
├── vocab.txt
└── config.yaml
```

## 发布模型包

桌面应用当前接受 `.tar.gz` 模型包，并通过模型注册表读取下载地址。可使用：

```bash
./scripts/publish_paraformer_zh.sh
```

该脚本默认会：
- 导出 `./models/paraformer-zh`
- 打包为 `.tar.gz`
- 更新 `public/model_registry.json`
- 上传到 GitHub Release

如果只想本地打包：

```bash
./scripts/publish_paraformer_zh.sh --package-only
```

## 备注

- 模型文件格式、下载地址和校验逻辑由当前原生项目直接使用
- `public/model_registry.json` 是当前模型注册表位置

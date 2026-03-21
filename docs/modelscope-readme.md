# Paraformer 中文语音识别 ONNX 模型

本仓库提供 Paraformer 中文语音识别模型的 ONNX 推理文件，基于 [FunASR](https://github.com/modelscope/FunASR) 框架导出，支持 CPU 直接推理。

## 模型文件

### paraformer-zh-streaming（流式识别）

实时流式语音识别模型，支持边说边转写。

| 文件 | 大小 | 说明 |
|------|------|------|
| `paraformer-zh-streaming-encoder.onnx` | ~607 MB | 编码器 |
| `paraformer-zh-streaming-decoder.onnx` | ~218 MB | 解码器 |
| `paraformer-zh-streaming-config.yaml` | ~3 KB | 模型配置 |
| `paraformer-zh-streaming-vocab.txt` | ~34 KB | 词表 |
| `paraformer-zh-streaming-am.mvn` | ~11 KB | 均值方差归一化参数 |

### paraformer-zh（离线识别）

非流式离线识别模型，对完整音频段进行整体识别，准确度更高。

| 文件 | 大小 | 说明 |
|------|------|------|
| `paraformer-zh-model.onnx` | ~825 MB | 完整离线模型 |
| `paraformer-zh-config.yaml` | ~2.5 KB | 模型配置 |
| `paraformer-zh-vocab.txt` | ~34 KB | 词表 |
| `paraformer-zh-am.mvn` | ~11 KB | 均值方差归一化参数 |

## 技术规格

- **推理格式**：ONNX（静态图，兼容 ONNX Runtime）
- **输入采样率**：16kHz 单声道 PCM
- **语言**：中文（普通话）
- **架构**：Paraformer（非自回归并行 Transformer）

## 相关链接

- FunASR：https://github.com/modelscope/FunASR
- Paraformer 论文：[Paraformer: Fast and Accurate Parallel Transformer for Non-autoregressive End-to-End Speech Recognition](https://arxiv.org/abs/2206.08317)

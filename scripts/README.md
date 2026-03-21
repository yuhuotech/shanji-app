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
    --model iic/speech_paraformer-large_asr_nat-zh-cn-16k-common-vocab8404-pytorch \
    --output ./models/paraformer-zh
```

### 3. 导出流式中文模型

```bash
python export_funasr_model.py \
    --model iic/speech_paraformer-large_asr_nat-zh-cn-16k-common-vocab8404-online \
    --output ./models/paraformer-zh-streaming
```

### 4. 直接上传流式 ONNX 文件到 GitHub Release

```bash
./scripts/publish_paraformer_zh_streaming.sh \
    --repo yuhuotech/paraformer-zh
```

### 5. 直接上传整体转写 ONNX 文件到 GitHub Release

```bash
./scripts/publish_paraformer_zh.sh \
    --repo yuhuotech/paraformer-zh
```

这两个发布脚本都不会打包 `.tar.gz`，而是：
- 导出模型
- 将文件重命名为统一标准命名
- 更新 `public/model_registry.json` 为 `backend + artifacts(role,fileName)` 结构
- 直接上传模型目录里的所有文件和 `model_registry.json`

## 支持的模型

| 模型 | ModelScope ID | 说明 |
|------|--------------|------|
| Paraformer 中文 | `iic/speech_paraformer-large_asr_nat-zh-cn-16k-common-vocab8404-pytorch` | 非流式，精度高 |
| Paraformer 流式中文 | `iic/speech_paraformer-large_asr_nat-zh-cn-16k-common-vocab8404-online` | 流式，延迟低 |
| Paraformer 英文 | `iic/speech_paraformer-large_asr_nat-en-16k-common-vocab10020` | 英语识别 |

## 输出文件结构

```text
models/paraformer-zh/
├── paraformer-zh-model.onnx
├── paraformer-zh-model-quant.onnx
├── paraformer-zh-am.mvn
├── paraformer-zh-config.yaml
└── paraformer-zh-vocab.txt
```

```text
models/paraformer-zh-streaming/
├── paraformer-zh-streaming-encoder.onnx
├── paraformer-zh-streaming-encoder-quant.onnx
├── paraformer-zh-streaming-decoder.onnx
├── paraformer-zh-streaming-decoder-quant.onnx
├── paraformer-zh-streaming-am.mvn
├── paraformer-zh-streaming-config.yaml
└── paraformer-zh-streaming-vocab.txt
```

## 备注

- 运行时不再依赖 `tokens.txt -> vocab.txt` 的特殊重命名
- 模型文件名和下载文件名保持一致，由 registry 中的 artifact role 驱动加载
- `public/model_registry.json` 是当前模型注册表位置
- 桌面应用支持全局网络代理配置，默认使用系统代理
- 自定义代理支持 `HTTP`、`HTTPS`、`SOCKS5`、`SOCKS5h`
- 模型下载、LLM 接口等所有网络请求共用同一套代理策略，并可在设置页测试连通性

# 闪记 (Shanji)

一款面向桌面端的 AI 语音输入工具，支持本地语音识别、可选 LLM 改写，以及自动粘贴到当前光标位置。

当前仓库是一个单一的 `Rust + Slint` 原生桌面项目。

![Version](https://img.shields.io/badge/version-0.1.0-blue)
![License](https://img.shields.io/badge/license-MIT-green)

## 功能特性

- 本地语音识别：基于 FunASR Paraformer ONNX，本地离线运行
- 智能改写：兼容 OpenAI API 风格接口，可选接入云端或本地 LLM
- 全局快捷键：支持 Toggle 和 Push-to-Talk 两种录音模式
- 自动粘贴：识别结果自动输出到当前光标位置
- 历史记录：SQLite 持久化存储，支持检索
- 自定义词库：支持导入热词与专业词汇
- 悬浮窗反馈：录音、识别、改写状态实时可见

## 系统要求

- macOS 12.0+
- Windows 10+
- Linux: Ubuntu 22.04 / Debian 12 或兼容发行版

## 当前状态

- 唯一目标应用：`Rust Core + Slint Native UI`
- 当前默认工作区：`shanji-core` + `shanji-platform` + 原生桌面 app
- 默认开发入口：Cargo

## 开发

### 当前主开发命令

当前默认开发流程已经切到 Cargo + 原生 app：

```bash
cargo run -p shanji-app
cargo check
cargo test -p shanji-core
cargo test -p shanji-app
cargo fmt --all
```

## 模型准备

闪记需要 FunASR Paraformer ONNX 模型才能运行语音识别：

```bash
python scripts/download_models.py
```

模型工具链独立于 UI 层，可单独维护。

## 项目结构

当前结构：

```text
shanji/
├── crates/
│   ├── shanji-core/        # 共享核心能力
│   ├── shanji-platform/    # 托盘、快捷键、剪贴板、输入模拟
│   └── shanji-slint/       # 当前唯一原生桌面应用（package: shanji-app）
├── docs/                   # 产品、设计与开发文档
├── scripts/                # 模型导出和下载工具
└── public/                 # 模型 registry 与词表等共享资源
```

## 技术栈

- UI：Slint
- 应用与业务：Rust
- ASR：FunASR Paraformer + ONNX Runtime
- 数据：SQLite + FTS5
- 构建：Cargo

## 文档

- [产品需求](./docs/prd.md)
- [技术设计](./docs/technical-design.md)
- [开发计划](./docs/dev_plan.md)
- [ASR 实现说明](./docs/asr-implementation.md)

## 许可证

MIT License

## 致谢

- [FunASR](https://github.com/alibaba-damo-academy/FunASR)
- [Slint](https://slint.dev/)
- [Paraformer](https://github.com/alibaba-damo-academy/FunASR/tree/main/examples/aishell/paraformer)

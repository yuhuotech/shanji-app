# 闪记 (Shanji)

一款面向桌面端的 AI 语音输入工具，支持本地语音识别、可选 LLM 改写，以及自动粘贴到当前光标位置。

当前仓库的唯一目标应用已经收敛为 `Rust + Slint` 原生桌面端。`src-tauri/` 和 `src/` 只作为旧实现参考源码，不再作为保留中的产品壳层或默认开发入口。

![Version](https://img.shields.io/badge/version-0.1.0-blue)
![License](https://img.shields.io/badge/license-MIT-green)

## 选型结论

下一阶段原生 UI 框架选择：`Slint`

选择原因：
- 不依赖 WebView，符合“去掉浏览器渲染层”的核心诉求
- UI 与业务逻辑都可以保持 Rust 实现，适合复用现有音频、ASR、模型管理、历史记录等模块
- 适合桌面实用工具场景，能覆盖主窗口、设置页、历史记录、悬浮窗等界面
- 支持桌面多平台，窗口能力足以支撑置顶、无边框 Overlay 等需求

不选择的方案：
- `Dioxus Desktop`：官方桌面文档仍说明使用系统 WebView 渲染，不符合本次迁移目标
- `egui/eframe`：更适合工具型或调试型界面，官方也明确不以“原生外观”作为目标
- `iced`：方向正确，但官方文档仍将其标注为 experimental；对本项目这种系统级桌面工具，当前不如 Slint 稳妥

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
- 当前默认工作区：`shanji-core` + 原生桌面 app
- `src-tauri/`：旧实现参考，不再是主开发入口

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

### 旧实现参考命令

以下命令仅在需要参考旧版 Tauri/WebView 行为时使用，不再代表当前主开发路径：

```bash
npm install
npm run tauri dev
cargo test --manifest-path src-tauri/Cargo.toml
```

## 模型准备

闪记需要 FunASR Paraformer ONNX 模型才能运行语音识别：

```bash
python scripts/download_models.py
```

模型工具链与 UI 框架无关，迁移到 Slint 后仍会继续沿用。

## 项目结构

当前结构：

```text
shanji/
├── crates/
│   ├── shanji-core/        # 共享核心能力
│   └── shanji-slint/       # 当前唯一原生桌面应用（package: shanji-app）
├── docs/                   # 产品、设计与迁移文档
├── scripts/                # 模型导出和下载工具
├── src-tauri/              # 旧 Tauri 实现参考源码
├── src/                    # 旧 WebView 前端参考源码
└── public/                 # 共享静态资源，例如模型 registry
```

目标结构：

```text
shanji/
├── crates/
│   ├── shanji-core/        # 音频、ASR、LLM、历史记录、配置、系统集成
│   ├── shanji-platform/    # 托盘、快捷键、剪贴板、输入模拟
│   └── shanji-app/         # 原生桌面应用入口与 UI 绑定
├── docs/
└── scripts/
```

## 技术栈

当前主实现：
- UI：Slint
- 应用与业务：Rust
- ASR：FunASR Paraformer + ONNX Runtime
- 数据：SQLite + FTS5
- 构建：Cargo

旧实现参考：
- 前端：React 18 + TypeScript + Tailwind CSS + Zustand
- 桌面壳层：Tauri 2.x

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

# 闪记（Shanji）技术设计文档

**版本**: v1.0  
**日期**: 2026-03-13

## 1. 设计目标

- 以 Rust 为唯一实现语言
- 以 Slint 为唯一桌面 UI
- 保持音频、ASR、改写、输出、历史、模型等核心能力解耦
- 让 UI、业务编排、平台集成边界清晰

## 2. 当前架构

```text
crates/
├── shanji-core/      # 配置、状态、音频、ASR、LLM、模型、历史、输出
├── shanji-platform/  # 托盘、快捷键、剪贴板、输入模拟等平台能力
└── shanji-slint/     # Slint UI、窗口绑定、应用编排、可执行入口
```

## 3. 分层职责

### 3.1 `shanji-core`

负责：

- 配置与路径
- 应用状态
- 音频采集与设备枚举
- ASR 推理
- LLM 改写
- 文本输出
- 历史记录
- 模型与热词数据

要求：

- 不依赖 Slint
- 不依赖具体平台窗口实现

### 3.2 `shanji-platform`

负责：

- 全局快捷键
- 系统托盘
- 剪贴板
- 输入模拟

要求：

- 不承担业务状态机
- 对外提供清晰的原生平台接口

### 3.3 `shanji-slint`

负责：

- 主窗口、设置窗口、历史窗口、Overlay
- UI 状态绑定
- 应用编排与窗口协调
- 调用 core 与 platform 完成用户动作

## 4. 运行主链路

```text
Global Hotkey
  -> Audio Capture
  -> ASR Partial / Final
  -> Optional Rewrite
  -> Output Delivery
  -> History Persist
  -> UI Refresh
```

## 5. 关键依赖

- UI：`slint`, `slint-build`
- 音频：`cpal`
- 推理：`ort`
- 数据：`rusqlite`
- HTTP：`reqwest`
- 异步：`tokio`
- 快捷键：`global-hotkey`
- 托盘：`tray-icon`
- 剪贴板：`arboard`
- 输入模拟：`enigo`

## 6. 窗口设计

### 6.1 主窗口

- 展示状态、模型、权限、录音测试和常用入口

### 6.2 设置窗口

- 管理录音、识别、改写、输出、快捷键等配置

### 6.3 历史窗口

- 查看、复制、回贴、删除历史记录

### 6.4 Overlay

- 展示录音、识别、改写和输出过程中的即时反馈

## 7. 工程要求

- `core` 不依赖 UI
- `platform` 不依赖具体业务模块
- 所有默认开发命令都以 Cargo 为中心
- 文档只描述当前原生项目

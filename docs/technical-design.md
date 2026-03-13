# 闪记（Shanji）技术设计文档

**版本**: v0.2  
**日期**: 2026-03-13  
**关联 PRD**: [prd.md](./prd.md)

## 1. 设计目标

本次技术调整的目标不是保留双壳并行，而是把现有桌面应用完整迁移为单一 `Rust + Slint` 原生应用。旧版 `Tauri + React + WebView` 只作为拆解和复用业务逻辑的参考来源，不再作为保留中的产品形态。

设计原则：

- 业务能力优先复用，不重造音频、ASR、历史记录、模型管理
- 以原生 app 为唯一入口，避免长期维护两套桌面壳
- UI 壳层替换优先，底层能力稳定后尽快删除旧依赖
- 文档必须区分“旧实现来源”和“当前主线架构”
- 系统级能力独立抽象，不把托盘、快捷键、粘贴逻辑绑死在 UI 框架内部

## 2. UI 选型结论

### 2.1 选择 `Slint`

结论：下一阶段桌面 UI 采用 `Slint`

原因：

- 不使用 WebView，符合迁移的根本动机
- 允许 UI 与业务逻辑都保持 Rust 实现
- 具备桌面应用所需的窗口能力，适合主窗口、设置页、历史页与 Overlay
- 对产品型桌面应用更合适，比即时模式 GUI 更利于维护完整界面状态
- 支持 Windows、macOS、Linux，且桌面后端覆盖 Wayland/X11

### 2.2 不采用的候选

`Dioxus Desktop`
- 官方桌面文档明确说明使用系统 WebView 渲染，不符合“摆脱 WebView”的要求

`egui/eframe`
- 官方定位更偏简单工具界面与即时模式交互，且明确不以原生外观为目标
- 对本项目的设置面板、历史列表、悬浮状态窗这类长期维护界面，并不是最优选择

`iced`
- 方向上是纯 Rust 原生 UI，但官方文档仍明确标注为 experimental
- 对系统级桌面工具而言，当前阶段风险和维护成本高于 Slint

## 3. 当前代码现状

仓库里仍能看到旧实现来源：

```text
src/         # 旧 WebView 前端参考源码
src-tauri/   # 旧 Tauri 实现参考源码
```

其中可复用的核心模块主要在 `src-tauri/src/`：

- `audio/`
- `asr/`
- `model/`
- `llm/`
- `history/`
- `hotwords/`
- `output/`
- `config/`

这些模块大多是 UI 无关逻辑，应被继续吸收到共享 core 或原生 app，而不是继续挂在旧壳上。

## 4. 目标架构

### 4.1 分层

```text
┌────────────────────────────────────────────┐
│              Slint Native UI               │
│  主窗口 / 设置 / 历史记录 / Overlay         │
└─────────────────────┬──────────────────────┘
                      │
             typed callbacks / shared state
                      │
┌─────────────────────▼──────────────────────┐
│               App Orchestrator             │
│  状态机、任务调度、窗口协调、事件分发         │
└───────┬──────────┬──────────┬──────────────┘
        │          │          │
        ▼          ▼          ▼
   Audio/ASR    LLM/Output   Config/History/Model
        │          │          │
        └──────────┴──────────┘
                   │
                   ▼
        System Integration Layer
   hotkeys / tray / clipboard / input simulate
```

### 4.2 目标目录

建议迁移后逐步收敛到：

```text
crates/
├── shanji-core/      # 业务核心能力
├── shanji-platform/  # 托盘、快捷键、剪贴板、输入模拟
├── shanji-ui/        # Slint UI 适配
└── shanji-app/       # 可执行入口
ui/
└── *.slint
```

在删除旧栈之前，可以继续从 `src-tauri/src/` 拆出逻辑；但这些代码的落点应是共享 core 或原生 app，而不是新的 Tauri 适配层。

## 5. 关键依赖建议

### 5.1 UI 与窗口

| 组件 | 建议 |
|------|------|
| 原生 UI | `slint` |
| UI 编译 | `slint-build` |
| 渲染后端 | 优先 `winit + skia`，保留软件渲染兜底 |

### 5.2 业务核心

| 组件 | 建议 |
|------|------|
| 音频采集 | `cpal` |
| 重采样 | `rubato` |
| ONNX 推理 | `ort` |
| HTTP | `reqwest` |
| 异步运行时 | `tokio` |
| 配置序列化 | `serde`, `serde_json` |
| 密钥存储 | `keyring` |
| 历史记录 | `rusqlite` |
| 文本输出 | `enigo` 或后续按平台抽象替换 |

### 5.3 系统集成

| 能力 | 建议 |
|------|------|
| 全局快捷键 | `global-hotkey` 为主；Linux Wayland 另行评估增强方案 |
| 系统托盘 | `tray-icon` |
| 文件对话框 | `rfd` 或平台封装 |
| 剪贴板 | `arboard` 或平台封装 |

说明：
- 托盘和全局快捷键不应由 UI 框架直接承担，而应作为独立平台层
- Wayland 对全局快捷键和输入模拟的约束是平台问题，不是 Slint 问题

## 6. 窗口设计

### 6.1 主窗口

职责：
- 展示工作状态
- 进入设置页、历史记录页
- 提供模型、麦克风、改写总开关等高频入口

### 6.2 设置窗口

职责：
- 录音设置
- 识别设置
- 词库管理
- LLM 配置
- 输出与粘贴策略
- 快捷键管理

### 6.3 历史记录窗口

职责：
- 按时间查看记录
- 搜索转写内容与改写内容
- 重新复制或重新粘贴

### 6.4 Overlay

职责：
- 录音中反馈
- 实时转写
- 改写进度
- 成功/失败结果提示

窗口要求：
- 无边框
- 置顶
- 尺寸小
- 可拖动
- 位置持久化

## 7. 状态流设计

### 7.1 录音状态机

```text
Idle
  -> Recording
  -> Finalizing
  -> Rewriting(optional)
  -> Outputting
  -> Idle
```

### 7.2 UI 更新流

目标是不再依赖 JS IPC，而是直接通过 Rust 状态与 Slint 回调驱动 UI：

```text
Audio input
  -> ASR partial
  -> App state update
  -> Overlay text refresh

ASR final
  -> optional LLM rewrite
  -> output/paste
  -> history persist
  -> UI completion state
```

### 7.3 推荐状态边界

- `core` 不依赖 Slint
- `ui` 不直接做音频和模型逻辑
- `app` 负责编排状态和线程/异步任务

## 8. 迁移策略

### Phase 1

- 保留现有 `src-tauri/src/` 业务逻辑
- 梳理与 Tauri 强耦合的部分：commands、events、window、tray
- 抽出可复用服务层接口

### Phase 2

- 创建最小 Slint 桌面壳
- 先接入主窗口和 Overlay 的静态 UI
- 用假数据跑通状态驱动

### Phase 3

- 迁移设置页与历史页
- 替换 Tauri 事件机制为 Rust 内部状态分发
- 接回音频、ASR、模型、LLM、历史记录

### Phase 4

- 替换托盘、快捷键、剪贴板、自动粘贴
- 清理 Tauri 配置、前端资源、Node 构建链

## 9. 风险

### 9.1 平台能力差异

- Linux 桌面环境差异会继续影响托盘与快捷键
- Wayland 对全局监听和自动输入存在天然限制

### 9.2 迁移复杂度

- 当前代码的 Tauri command/event 边界需要重新设计
- 原先依赖前端状态的设置页逻辑需要改为 Rust 状态驱动

### 9.3 打包与更新

- 现有 Tauri 打包配置不可直接复用
- 自动更新机制需要重新评估实现方式

## 10. 当前决定

- UI 选型固定为 `Slint`
- 现有 Rust 核心能力优先保留
- 托盘、快捷键、剪贴板等系统能力单独抽象
- 本次文档更新后，再进入实际代码迁移阶段

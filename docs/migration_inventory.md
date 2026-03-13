# 迁移清单

**日期**: 2026-03-13

## 1. 当前模块分层

说明：

- `src-tauri/` 现阶段仅作为旧实现拆解来源，不再是保留中的桌面壳
- 当前唯一目标应用是原生 Slint app

### 可优先抽离到共享 core 的模块

- `error.rs`
- `config/mod.rs`
- 与应用目录相关的路径解析逻辑

### 下一批适合抽离的模块

- `state.rs`
- `history/mod.rs` 中与数据库路径解析相关部分
- `hotwords/mod.rs` 中与数据目录相关部分

### 尚未拆解完的旧壳层模块

- `commands/*`
- `events.rs`
- `windows/mod.rs`
- `tray.rs`
- `hotkeys.rs`
- `main.rs`

## 2. 当前 Tauri 强耦合点

- `AppHandle` 被直接用于配置、历史记录、模型、词库、输出、音频流程
- 事件推送通过 `app.emit(...)`
- 悬浮窗、设置窗、历史窗依赖 `WebviewWindowBuilder`
- 托盘依赖 Tauri tray/menu API
- 快捷键依赖 `tauri-plugin-global-shortcut`

## 3. 已完成的首批迁移

- 新建 workspace 根 `Cargo.toml`
- 已将 workspace 默认入口切到原生 app，并移除 `src-tauri/` 默认成员身份
- 新建共享 crate `crates/shanji-core`
- 抽离 `error`、`paths`、`config`
- 抽离 `state` 到共享 core，并保留 Tauri 事件 wrapper
- 现有 Tauri `config` 模块改为 wrapper
- `history` 与 `hotwords` 的目录访问已切到共享 `AppPaths`
- 新建最小 Slint 原生入口 `crates/shanji-slint`
- Slint 主窗口已能读取共享配置与共享运行时状态
- Slint 主窗口已具备最小配置写回交互（theme / recording mode）
- Slint 多窗口方向已验证：最小 Overlay 原生窗口已创建
- Slint 主窗口与 Overlay 已能随共享 `AppState` 同步更新文案
- `history` 已下沉到 `shanji-core`，Slint 可直接读取最近记录摘要
- Slint 已开始承担真实设置面板职责，而不再只是状态演示壳
- `hotwords` 已下沉到 `shanji-core`，Slint 可直接读取启用词库摘要
- `model` 已具备共享只读能力，Slint 可直接读取模型摘要与当前激活模型
- 标准应用路径解析已下沉到 `shanji-core::paths`，Slint 不再自行拼接目录
- `model` 的 registry 拉取 fallback、下载状态判定、删除、本地导入、配置切换路径逻辑已下沉到 `shanji-core`
- Slint 主窗口已具备原生服务动作：可直接切换当前模型、切换最新热词库启停、清理历史记录
- 共享 `state` 已扩展出 native runtime 快照，承载 Overlay 可见性、实时转写、改写预览、最终输出和音量级别
- 原生 app 已具备最小 session flow，推进一次完整状态流时会把结果写入共享历史数据库
- 麦克风输入设备枚举已从旧实现迁入 `shanji-core::audio`，原生 app 可直接读取真实设备列表摘要
- 基础音频采集 `AudioCapture` 与音量计算已迁入 `shanji-core::audio`，原生 app 可直接启停真实麦克风监测
- 旧版 ASR 引擎已接入 `shanji-core::asr`，原生 app 可通过 `Live ASR` 直接跑真实麦克风 + 模型的 partial/final 识别
- LLM 改写与输出格式化/剪贴板写入已接入共享 core，原生 app 的 `Live ASR` 尾链路已开始直接消费这些能力
- 原生主窗口已去掉 demo runtime 推进按钮，改为真实音频设备切换、会话清理，以及 mic monitor / live ASR 互斥控制
- 已新增 `crates/shanji-platform`，把快捷键与托盘先收敛为原生平台抽象，不再让下一步迁移直接依赖旧 Tauri API 形状
- `shanji-core::asr` 已切换为本地模块路径，不再通过 `#[path = ../../../src-tauri/... ]` 依赖旧壳源码
- 原生 app 已接入真实 `global-hotkey` runtime，能直接把全局快捷键事件分发到当前 `Live ASR` / push-to-talk / rewrite / open-main 主流程
- 原生 app 已接入真实 `tray-icon` runtime，托盘菜单与左键主动作已能直接驱动当前原生主窗口和录音入口
- 原生输出链路已恢复为“格式化 -> 剪贴板 -> 自动粘贴 -> 失败回退剪贴板”模式，不再停留在仅复制文本
- 原生 app 已具备独立 `HistoryWindow`，历史查看和“回贴最新记录”不再依赖旧 WebView 页面或 Tauri 命令入口

## 4. 下一步

- 开始把 `audio` / `asr` / `llm` 主链路整理为共享服务边界
- 把 Slint Overlay 从演示态接到真实录音与转写状态
- 逐步替换 Tauri 的窗口、托盘、快捷键与系统集成
- 在原生 app 完成主链路后，直接删除 `src/` 与 `src-tauri/` 旧壳代码

# 闪记（Shanji）迁移开发计划

**版本**: v0.3  
**日期**: 2026-03-13  
**目标**: 完整迁移为单一 `Rust + Slint` 原生桌面应用，不再保留 Tauri 壳层

## 1. 迁移原则

- 不保留双壳并存，原生 app 作为唯一目标入口
- 先抽共享 Rust 核心，再把旧壳层逻辑搬入原生 app
- 每一步都保持仓库可编译，避免一次性大搬家
- 先解耦路径、配置、状态、服务接口，再迁移窗口和系统集成
- 任何阶段都不能牺牲现有核心能力：录音、ASR、改写、粘贴、模型、历史记录

## 2. 迁移路线

### Stage 0: 基线冻结

目标：
- 明确当前 Tauri 实现只是拆解素材，不再作为并行维护的应用壳

产出物：
- 架构文档
- 迁移计划
- 迁移边界说明

状态：
- 已完成

### Stage 1: 建立共享 Core 层

目标：
- 把未来 Slint 和当前 Tauri 都需要的基础能力抽成独立 Rust crate

拆分顺序：
1. `error`
2. `paths`
3. `config`
4. `state`
5. 纯业务类型定义

验收标准：
- 新增独立 crate
- 当前 Tauri 应用通过 path dependency 复用该 crate
- 至少配置与路径解析不再依赖 Tauri 内部结构

状态：
- 进行中
- 已完成 Step 1：共享 `shanji-core` crate 已建立，`error`、`paths`、`config` 已抽离

### Stage 2: 拆解旧壳耦合边界

目标：
- 明确哪些模块要进入共享 core，哪些模块要被原生 app 接管

模块分层：
- 可复用：`audio/`、`asr/`、`llm/`、`model/`、`history/`、`hotwords/`
- 待重构：`state.rs`
- 待删除旧壳层：`commands/`、`events.rs`、`windows/`、`tray.rs`、`hotkeys.rs`、`main.rs`

验收标准：
- 输出模块迁移映射表
- 为高耦合模块建立原生替换接口

状态：
- 进行中
- 已完成最小原生入口：`crates/shanji-slint` 已创建并通过编译
- 已完成首批共享边界抽离：`history`、`hotwords`、`model` 已形成共享 core 能力
- 已将根 workspace 默认入口切换到原生 app，`src-tauri/` 不再属于默认工作区
- 已新增 `crates/shanji-platform` 作为原生系统集成抽象层，开始承接 hotkeys / tray 的迁移边界
- 已接入首个真实原生系统能力：`global-hotkey` 已通过 `shanji-platform` 接到当前 Slint app，支持 toggle / push-to-talk / rewrite / open-main / open-history 动作分发
- 已接入 `tray-icon` 与原生输出自动粘贴链路，当前原生 app 已具备托盘菜单、托盘主动作、以及“自动粘贴失败时回退到剪贴板”的完整输出处理
- 已新增原生 `HistoryWindow`，并把热键/托盘里的 `Open History` 接到真实窗口；历史窗口已支持刷新、回贴最新记录、复制最新记录、删除最新记录、清空历史

### Stage 3: 建立原生桌面应用主干

目标：
- 创建并持续扩展唯一原生应用入口

任务：
- 建立 `shanji-app` crate
- 引入 `slint`、`slint-build`
- 创建主窗口占位 UI
- 建立共享状态到 UI 的最小绑定链路

验收标准：
- 有独立的原生可执行入口
- 能在本地启动一个原生窗口

状态：
- 待开始
- 已建立 `shanji-platform` 抽象骨架，并在原生主窗口显示 hotkeys / tray 摘要

### Stage 4: 迁移 UI 状态与导航

目标：
- 把当前前端页面状态和事件流迁移到 Rust + Slint

任务：
- 主窗口导航
- 设置页框架
- 历史页框架
- Overlay 静态界面
- 应用状态枚举和 UI 状态同步

验收标准：
- 不依赖 JS IPC 的 UI 刷新链路成型

状态：
- 待开始

### Stage 5: 回接配置、模型、历史、词库

目标：
- 先恢复管理类能力

任务：
- 设置页回接配置读写
- 模型列表/切换
- 历史记录查询/复制/粘贴
- 热词库管理

验收标准：
- Slint 壳下可完成非录音类核心操作

状态：
- 待开始

### Stage 6: 回接音频、ASR、改写、输出

目标：
- 恢复完整的“说话即输入”主链路

任务：
- 麦克风枚举与测试
- 录音状态机
- VAD
- 流式 ASR
- 流式改写
- 自动粘贴

验收标准：
- 在 Slint 壳中完成完整录音到输出流程

状态：
- 待开始

### Stage 7: 替换系统能力

目标：
- 去掉 Tauri 平台插件依赖

任务：
- 用 `tray-icon` 替换托盘
- 用 `global-hotkey` 替换全局快捷键
- 用平台封装替换剪贴板/文件选择等能力

验收标准：
- 主流程不再依赖 Tauri plugin

状态：
- 待开始

### Stage 8: 删除旧栈

目标：
- 删除不再需要的 Tauri / WebView / Node 相关代码和构建链

任务：
- 删除 `src/` Web 前端
- 删除 Tauri 配置与插件
- 删除 Node/Vite/Tailwind 依赖
- 收敛到 Cargo 主导的构建流程

验收标准：
- 仓库默认开发入口切换为 Rust/Slint

状态：
- 待开始

## 3. 当前执行顺序

### Step 1

内容：
- 新增 workspace
- 新增 `crates/shanji-core`
- 抽离 `error`、`paths`、`config`
- 让现有 Tauri 配置读写改为依赖 core crate

验收：
- 共享 core 已建立，后续不再以保留 Tauri 适配层为目标

状态：
- 已完成

### Step 2

内容：
- 输出 Tauri 耦合清单
- 标出下一批可抽离模块：`state`、`history` 路径访问、`hotwords` 路径访问

状态：
- 进行中
- 已完成目录访问迁移：`history`、`hotwords` 已改为通过共享 `AppPaths` 取路径

### Step 3

内容：
- 建立最小 Slint app crate

状态：
- 已完成最小骨架
- 已接入共享 `config` 与 `state` 的只读展示
- 已接入最小写回交互：主题和录音模式可在 Slint 壳中修改并写入共享配置
- 已建立最小原生 Overlay 骨架，并由主窗口控制显示/隐藏
- 已打通共享 `AppState` 到主窗口与 Overlay 的联动刷新链路
- 已将 `history` 数据库能力抽到共享 core，并接入 Slint 主窗口摘要展示
- 已把更多真实设置项接入 Slint：rewrite、punct style、append content 可原生写回
- 已将 `hotwords` 能力抽到共享 core，并接入 Slint 主窗口摘要展示
- 已新增共享 `model` 只读能力，并将 Slint 主窗口重组为更清晰的原生设置首页
- 已统一标准路径解析到 core，并支持在 Slint 中切换当前激活模型
- 已将 `model` 的远端 registry fallback、下载状态判断、删除、切换配置、本地导入路径逻辑下沉到共享 core
- 已将 Slint 主窗口扩展为原生服务总览，并支持直接清理历史记录、切换最新热词库启停状态
- 已将原生 runtime 快照并入共享 `state`，主窗口与 Overlay 开始围绕同一份 native session 状态刷新
- 已把一条最小原生 session flow 接到共享历史：录音 -> 转写 -> 改写 -> 完成 可由原生 app 推进并落库
- 已把麦克风输入设备枚举迁入共享 core，原生 app 可直接展示真实音频设备摘要
- 已将基础音频采集能力迁入共享 core，原生 app 可直接启停真实麦克风监测并更新 runtime 音量
- 已将 ASR 引擎接入共享 core，并在原生 app 中新增真实 `Live ASR` 开关，开始跑麦克风 + 模型的 partial/final 主链路
- 已将 LLM 改写与输出格式化接入共享 core，`Live ASR` 停止后可按配置执行改写并把最终结果写入系统剪贴板
- 已移除 demo `Advance Runtime` 依赖，主窗口动作改为真实设备切换、会话清理，以及 mic monitor / live ASR 互斥控制

## 4. 迁移完成定义

- 默认 UI 为 Slint
- 录音、ASR、改写、输出、模型、历史、词库全部可用
- 不再依赖 Tauri WebView 与前端 Node 构建链
- 仓库主开发流程收敛到 Cargo

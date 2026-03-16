# 统一设置窗口设计规格

**日期**：2026-03-16
**状态**：已确认
**范围**：将 `SettingsWindow` 和 `HistoryWindow` 合并为单一窗口，完全重构 UI

---

## 1. 背景与目标

### 现状问题

- `SettingsWindow` 和 `HistoryWindow` 是两个独立的 `Window` 组件，分别管理，逻辑重复
- 设置窗口仅由一堆"点击循环切换"的按钮和文本标签组成，不是真正意义上的设置 UI
- 历史窗口只显示一段拼接的文本，没有逐条记录的交互

### 目标

- 合并为单一 `SettingsWindow`，内部通过 `active-section` enum 属性切换页面
- 重构为经典左右布局：左侧固定导航栏，右侧内容区
- 右侧控件改为标准设置表单：下拉选择器、开关（Toggle）、分段控件、文本输入框
- 历史记录作为设置窗口的一个菜单项，但有独立入口
- 为将来的浅色/深色模式切换预留扩展空间

---

## 2. 窗口规格

| 属性 | 值 |
|------|-----|
| 组件名 | `SettingsWindow`（替换原有两个组件） |
| 尺寸 | 900 × 620 px |
| 默认字体 | Hiragino Sans GB |
| 布局 | HorizontalLayout：左侧边栏（180px）+ 右侧内容区（剩余宽度） |
| 配色基调 | 浅灰白（侧边栏 `#f3f4f6`，内容区 `#ffffff`，分组卡片 `#f9fafb`） |

---

## 3. 侧边栏导航

宽度固定 **180px**，背景 `#f3f4f6`，右侧 1px 分割线 `#e5e7eb`。

### 顶部 App 信息区

- 应用名称「闪记」，粗体，`#111827`
- 副文字：版本号（如 `v0.1 · 设置`），`#9ca3af`
- 底部 1px 分割线

### 菜单项列表

| 顺序 | 图标 | 标签 | SettingsSection 枚举值 |
|------|------|------|------------------------|
| 1 | 🎙 | 语音识别 | `Asr` |
| 2 | ✨ | LLM 润色 | `Llm` |
| 3 | ⌨️ | 输出 | `Output` |
| 4 | 🔑 | 快捷键 | `Hotkeys` |
| 5 | 🌐 | 网络 | `Network` |
| 6 | 🕑 | 历史记录 | `History` |
| — | — | （弹性空白） | — |
| 7 | ℹ️ | 关于 | `About` |

「关于」置底，通过弹性空白与其他项分隔。

**活跃状态**：选中项背景 `#e5e7eb`，文字 `#111827`，font-weight 600，圆角 6px，左右各留 8px margin。
**非活跃状态**：文字 `#6b7280`，无背景。

---

## 4. `SettingsSection` 枚举

Slint 中定义：

```slint
enum SettingsSection { Asr, Llm, Output, Hotkeys, Network, History, About }
```

`SettingsWindow` 使用 `in-out property <SettingsSection> active-section: SettingsSection.Asr` 控制当前页面。条件渲染写法：`if root.active-section == SettingsSection.Asr: AsrPage { ... }`。

---

## 5. 右侧内容区——各页面规格

右侧背景 `#ffffff`，内边距 `20px 24px`。每页顶部：大标题（`font-size: 16px, font-weight: 700`）+ 副标题描述（`font-size: 11px, color: #9ca3af`）。

设置项按小节组织：小节标题（全大写灰色标签）+ 圆角分组卡片（`background: #f9fafb, border: 1px solid #f3f4f6, border-radius: 8px`）。

### 5.1 语音识别（Asr）

**模型** 小节：
- 实时模型：下拉选择器，列出已下载的 streaming 模型
- 整体纠正：Toggle 开关 + 副文字说明"录音结束后二次修正"

**音频** 小节：
- 输入设备：下拉选择器，列出可用麦克风
- 录音模式：分段控件（Toggle / Hold）

**热词库** 小节：
- 热词库开关：Toggle 开关，`toggle-hotword-requested()` callback
- 热词库摘要：只读文字，显示当前加载的热词库名称列表

**模型管理** 小节：
- 下载实时模型：按钮 + 进度状态文字
- 下载纠正模型：按钮 + 进度状态文字

### 5.2 LLM 润色（Llm）

**顶部** 单行 Toggle：启用 LLM 润色。

- Slint 侧：`in-out property <bool> llm-enabled`
- 下方所有输入控件使用 `enabled: root.llm-enabled` 实现灰色禁用（保持布局，仅视觉变灰 + 不可交互）

**API 配置** 小节：
- API Base URL：文本输入框，初始值由 `llm-base-url` property 填充；失焦时触发 `set-llm-base-url(string)` callback
- API Key：密码输入框（`input-type: password`），初始值显示掩码；修改后触发 `set-llm-api-key(string)` callback；副文字"存储在系统钥匙串"
- 模型名称：文本输入框，失焦时触发 `set-llm-model-name(string)` callback

**润色提示词** 小节：
- 多行文本输入框（`TextEdit`），失焦时触发 `set-llm-system-prompt(string)` callback

**所需 Slint properties**：
```
in-out property <bool> llm-enabled
in-out property <string> llm-base-url
in-out property <string> llm-model-name
in-out property <string> llm-system-prompt
```
（API Key 不回显明文，property 仅用于判断是否已设置）

### 5.3 输出（Output）

**格式** 小节：
- 标点风格：分段控件（zh / en / none）
- 追加内容：分段控件（空格 / 换行 / 无）

**界面** 小节：
- 悬浮窗：Toggle 开关

**外观** 小节：
- 主题：分段控件（浅色 / 深色 / 跟随系统）；`cycle-theme-requested()` callback 保留，或改为 `set-theme(string)` callback

### 5.4 快捷键（Hotkeys）

**Push-to-Talk** 小节：
- 当前快捷键：只读文字显示当前绑定（`hotkey-summary-text` property）
- 「重新录制」按钮
- 状态说明：不可用时显示原因

### 5.5 网络（Network）

**下载代理** 小节：
- GitHub 代理：分段控件（ghfast.top / gh-proxy.com / 直连）；`cycle-github-proxy-requested()` callback

### 5.6 历史记录（History）

#### 数据模型

在 `settings-window.slint` 中定义 Slint struct：

```slint
struct HistoryCardData {
    record-id: int,          // i32，SQLite rowid 实际不超过 2^31 在此场景下安全
    timestamp: string,       // 格式化后的时间字符串，如 "2026-03-16 14:32"
    text: string,            // 完整转录文本
    has-audio: bool,         // 是否存有录音文件
    is-llm-rewritten: bool,  // 是否经过 LLM 润色
    was-pasted: bool,        // 是否已自动粘贴
}
```

`SettingsWindow` 新增：

```slint
in property <[HistoryCardData]> history-records;   // 由 Rust 侧推送
in property <string> history-stats-text;            // "共 N 条 · 最近 20 条"
```

#### UI 布局

**顶部操作栏**：
- 左侧：`history-stats-text` 统计文字
- 右侧：「刷新」按钮（`refresh-history-requested()`）+ 「清空全部」按钮（红色，`clear-history-requested()`）

**卡片列表**（`ScrollView`，垂直滚动，`for card in root.history-records`）：

每张卡片包含：
- 顶部：`card.timestamp`（左）+ 操作按钮组（右）
- 正文：`card.text`，`wrap: word-wrap`
- 底部标签：LLM润色 / 原始转录 + 已粘贴（`card.was-pasted`）+ 🎙 有录音（`card.has-audio`）

**操作按钮组**（从左到右，均传 `card.record-id`）：

| 按钮 | 可用条件 | Callback |
|------|----------|----------|
| ▶ 播放录音 | `card.has-audio` | `play-audio-requested(int)` |
| ↻ 重新转录 | `card.has-audio` | `retranscribe-requested(int)` |
| 复制 | 始终 | `copy-record-requested(int)` |
| 粘贴 | 始终 | `paste-record-requested(int)` |
| 删除 | 始终（红色） | `delete-record-requested(int)` |

不可用时按钮显示灰色，`enabled: card.has-audio`。

#### 显示条数

固定显示最近 **20 条**（与现有 `db.list(0, 20)` 保持一致）。`history-stats-text` 中总数由 Rust 侧格式化后传入。

### 5.7 关于（About）

- 应用版本（只读文字）
- 配置文件路径（只读文字，可复制）
- 「清理会话」危险操作按钮（红色，`clear-session-requested()` callback）

---

## 6. 两个入口点行为

| 触发 | 行为 |
|------|------|
| 主窗口「设置」按钮 | 打开/聚焦窗口，`active-section = SettingsSection.Asr` |
| 主窗口「历史记录」按钮 | 打开/聚焦窗口，`active-section = SettingsSection.History` |
| 系统托盘「历史记录」 | 打开/聚焦窗口，`active-section = SettingsSection.History` |
| 全局热键 `OpenHistory` | 打开/聚焦窗口，`active-section = SettingsSection.History` |

---

## 7. Slint 组件结构

```
SettingsWindow (inherits Window, 900×620)
├── HorizontalLayout
│   ├── SidebarNav (width: 180px)
│   │   ├── AppHeader
│   │   ├── NavItem × 6  (普通菜单项)
│   │   ├── 弹性空白 (VerticalLayout stretch)
│   │   └── NavItem × 1  (关于，置底)
│   └── ContentArea (flex: 1, ScrollView 包裹各 Page)
│       ├── AsrPage       (if active-section == Asr)
│       ├── LlmPage       (if active-section == Llm)
│       ├── OutputPage    (if active-section == Output)
│       ├── HotkeysPage   (if active-section == Hotkeys)
│       ├── NetworkPage   (if active-section == Network)
│       ├── HistoryPage   (if active-section == History)
│       └── AboutPage     (if active-section == About)
```

---

## 8. Rust 侧接口变更

### 移除

- `HistoryWindow` Slint 组件及 `history-window.slint`
- `HistoryWindowSnapshot` struct
- `refresh_history_window()` 函数
- `main.rs` 中的 `history_window` 变量及相关事件绑定
- 旧版 `play_latest_history_audio()` / `copy_latest_history()` / `paste_latest_history()` / `delete_latest_history()`（替换为按 ID 操作的新函数）

### 新增

**`shanji-core` 新增可导出的 Slint 兼容 Rust struct**（用于填充 `VecModel`）：

```rust
// shanji-core/src/history.rs 或新建 shanji-core/src/history_card.rs
pub struct HistoryCardData {
    pub record_id: i32,
    pub timestamp: String,
    pub text: String,
    pub has_audio: bool,
    pub is_llm_rewritten: bool,
    pub was_pasted: bool,
}
```

**`app.rs` 新增函数**：

```rust
pub fn load_history_cards() -> Vec<HistoryCardData>   // 最近 20 条
pub fn play_history_audio(record_id: i32) -> Result<(), String>
pub fn retranscribe_history(record_id: i32) -> Result<(), String>
pub fn copy_history_record(record_id: i32) -> Result<(), String>
pub fn paste_history_record(record_id: i32) -> Result<(), String>
pub fn delete_history_record(record_id: i32) -> Result<(), String>
pub fn set_llm_base_url(url: String) -> Result<(), String>
pub fn set_llm_api_key(key: String) -> Result<(), String>
pub fn set_llm_model_name(model: String) -> Result<(), String>
pub fn set_llm_system_prompt(prompt: String) -> Result<(), String>
```

**`main.rs` 修改**：
- `handle_platform_hotkey_event` 和 `handle_platform_tray_event` 移除 `&HistoryWindow` 参数
- `HotkeyAction::OpenHistory` 和 `TrayAction::OpenHistory` 改为调用 `open_settings_window_at(SettingsSection::History)`
- `open_history_window()` 改为 `open_settings_window_at(section: SettingsSection)`

### 保留（迁移）

- `clear_history()` 保留，callback 移至 `SettingsWindow`
- `toggle-hotword-requested()` callback 保留，迁移至语音识别页
- 所有模型下载 callbacks 保留，迁移至语音识别页

---

## 9. 文件变更清单

| 文件 | 变更类型 | 说明 |
|------|----------|------|
| `crates/shanji-slint/ui/settings-window.slint` | 重写 | 新的左右布局，包含全部 7 个页面，定义 SettingsSection enum 和 HistoryCardData struct |
| `crates/shanji-slint/ui/history-window.slint` | 删除 | 功能迁移至 settings-window.slint |
| `crates/shanji-slint/src/main.rs` | 修改 | 移除 HistoryWindow；更新 handle_platform_hotkey_event、handle_platform_tray_event；新增 open_settings_window_at |
| `crates/shanji-slint/src/app.rs` | 修改 | 新增按 ID 操作的历史函数；新增 LLM 配置写入函数；新增 load_history_cards() |
| `crates/shanji-slint/ui/app-window.slint` | 修改 | 历史按钮 callback 改为调用 settings，section=History |
| `crates/shanji-core/src/history.rs` | 修改 | 新增 HistoryCardData struct；新增按 ID 查询/删除函数 |

---

## 10. 扩展预留（深色模式）

配色全部使用 `Palette` global，不硬编码颜色值。将来切换深色模式时只需在 Rust 侧写入 `in-out property` 值。

```slint
global Palette {
    in-out property <brush> sidebar-bg: #f3f4f6;
    in-out property <brush> content-bg: #ffffff;
    in-out property <brush> card-bg: #f9fafb;
    in-out property <brush> border: #f3f4f6;
    in-out property <brush> text-primary: #111827;
    in-out property <brush> text-secondary: #6b7280;
    in-out property <brush> text-muted: #9ca3af;
    in-out property <brush> nav-active-bg: #e5e7eb;
    in-out property <brush> accent: #2563eb;
    in-out property <brush> danger: #dc2626;
}
```

切换深色模式时，Rust 侧批量更新上述 `in-out property` 值，无需修改组件结构代码。

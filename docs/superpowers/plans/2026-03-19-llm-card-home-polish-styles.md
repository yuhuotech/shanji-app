# LLM润色卡片 + 润色风格选项 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把首页第三张卡片替换为 LLM 润色直接操作卡（开关+风格 chips），麦克风授权降级为状态条，新增 4 个内置润色风格。

**Architecture:** 新增 3 个内置 PromptPreset 常量 + v10 迁移；`UiSnapshot`/`SettingsWindowSnapshot` 增加 `llm_active_prompt_id`/`llm_provider_summary`；首页卡片和设置页 LLM 区域同步展示风格选择 chips。

**Tech Stack:** Rust (shanji-core, shanji-slint), Slint UI

---

## 涉及文件

| 文件 | 改动 |
|---|---|
| `crates/shanji-core/src/config.rs` | 新增 3 个 prompt 常量；`default()` 加入 3 个内置预设；v10 迁移 |
| `crates/shanji-slint/src/app.rs` | `UiSnapshot`/`SettingsWindowSnapshot` 新增字段；`refresh_snapshot`/`refresh_settings_window` 填充；新增 `set_active_prompt` fn |
| `crates/shanji-slint/src/main.rs` | `apply_snapshot` 传新字段；新增两个 callback handler |
| `crates/shanji-slint/ui/app-window.slint` | 新属性/callbacks；首页麦克风降级为状态条；Card 3 换成 LLM 润色卡；compact layout 同步 |
| `crates/shanji-slint/ui/settings-window.slint` | LLM 页新增风格 chips；新属性/callback |

---

### Task 1: config.rs — 新增 3 个内置润色预设 + v10 迁移

**Files:**
- Modify: `crates/shanji-core/src/config.rs`

- [ ] 在 `DEFAULT_REWRITE_SYSTEM_PROMPT` 常量后添加 3 个新常量
- [ ] 在 `RewriteConfig::default()` 的 prompts 里加入 3 个新 `PromptPreset`
- [ ] 在 `migrate()` 末尾添加 `version < 10` 分支，注入 3 个新内置预设（如果不存在）

Run: `cargo check -p shanji-core`

---

### Task 2: app.rs — 快照结构体扩展 + set_active_prompt

**Files:**
- Modify: `crates/shanji-slint/src/app.rs`

- [ ] `UiSnapshot` 增加 `llm_active_prompt_id: String`, `llm_provider_summary: String`
- [ ] `SettingsWindowSnapshot` 增加 `llm_active_prompt_id: String`
- [ ] `bootstrap_snapshot()` error 分支填默认值
- [ ] `refresh_snapshot()` 填充两个新字段
- [ ] `refresh_settings_window()` 填充 `llm_active_prompt_id`
- [ ] 新增 `pub fn set_active_prompt(id: String) -> Result<UiSnapshot, String>`

Run: `cargo check -p shanji-app`

---

### Task 3: main.rs — 连接新 callbacks + 传递新字段

**Files:**
- Modify: `crates/shanji-slint/src/main.rs`

- [ ] `apply_snapshot` 传 `llm_active_prompt_id` 和 `llm_provider_summary`
- [ ] `apply_settings_snapshot` 传 `llm_active_prompt_id`
- [ ] 新增 `on_set_active_prompt_requested` callback
- [ ] 新增 `on_open_llm_settings_requested` callback

Run: `cargo check -p shanji-app`

---

### Task 4: app-window.slint — 首页 UI 改造

**Files:**
- Modify: `crates/shanji-slint/ui/app-window.slint`

- [ ] 添加新属性: `llm-active-prompt-id`, `llm-provider-summary`
- [ ] 添加新 callbacks: `set-active-prompt-requested(string)`, `open-llm-settings-requested()`
- [ ] 在 SettingsPage 绑定 forward 这两个新属性/callbacks
- [ ] 宽布局 hotkey banner 下方加麦克风状态条
- [ ] 宽布局 Card 3 替换为 LLM 润色卡（开关 + provider summary + 风格 chips + 配置详情）
- [ ] compact 布局同步修改

Run: `cargo check -p shanji-app`

---

### Task 5: settings-window.slint — LLM 页加风格 chips

**Files:**
- Modify: `crates/shanji-slint/ui/settings-window.slint`

- [ ] 新增属性: `llm-active-prompt-id`, callback `set-active-prompt-requested(string)`
- [ ] 在润色提示词 section 上方加风格 chips 选择行

Run: `cargo check -p shanji-app`

---

### Task 6: 最终构建验证

- [ ] `cargo build -p shanji-app`
- [ ] `cargo fmt --all`
- [ ] 提交

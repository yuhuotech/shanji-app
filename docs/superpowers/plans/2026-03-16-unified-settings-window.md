# 统一设置窗口 Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 SettingsWindow 和 HistoryWindow 合并为单一窗口，重构为经典左右侧边栏布局，历史记录成为设置窗口的一个菜单项。

**Architecture:** 新的 `SettingsWindow` 使用 `SettingsSection` enum 驱动左侧导航，右侧通过 Slint `if` 条件渲染切换各页面。历史页使用 `VecModel<HistoryCardData>` 渲染逐条交互卡片，LLM 页改为表单控件（文本输入 + 开关）。`HistoryWindow` 组件及相关 Rust 接口全部删除，功能迁移至 `SettingsWindow`。

**Tech Stack:** Rust, Slint UI, rusqlite (history), keyring (API key), cpal (audio playback), shanji-core

---

## Chunk 1: Core 层 — HistoryCardData + 按 ID 操作

### Task 1: 在 shanji-core 添加 `HistoryCardData` 及按 ID 的历史操作函数

**Files:**
- Modify: `crates/shanji-core/src/history.rs`

- [ ] **Step 1: 在 `history.rs` 末尾已有的 `#[cfg(test)]` 块之前添加 `HistoryCardData` struct 和 `delete_by_id` 方法**

在 `HistoryDb` impl 的 `delete` 方法下方已有按 `i64` 操作的 `delete`，但我们需要暴露按 `i32`（Slint `int`）操作的函数，以及 `HistoryCardData` 用于 UI 展示。

在 `history.rs` 的 `use` 区域之后、`HistoryRecord` struct 之前，添加：

```rust
/// 用于 Slint UI 渲染的历史卡片数据（扁平化，无 Option）
#[derive(Debug, Clone)]
pub struct HistoryCardData {
    pub record_id: i32,
    pub timestamp: String,    // 格式化后的本地时间，如 "2026-03-16 14:32"
    pub text: String,         // 优先展示: rewritten > corrected_transcribed > transcribed
    pub has_audio: bool,
    pub is_llm_rewritten: bool,
    pub was_pasted: bool,     // 当前始终 false（暂无粘贴状态跟踪，预留字段）
}
```

在 `HistoryDb` impl 块内，`delete` 方法之后添加：

```rust
/// 取最近 20 条记录并转换为 UI 卡片格式
pub fn list_cards(&self, limit: u32) -> Result<(Vec<HistoryCardData>, u32)> {
    let total = self.count()?;
    let records = self.list(0, limit)?;
    let cards = records.into_iter().filter_map(|r| {
        let id = r.id? as i32;
        // 注意：先捕获 is_llm_rewritten，再 move rewritten 字段
        let is_llm_rewritten = r.rewritten.as_ref()
            .map(|s| !s.is_empty()).unwrap_or(false);
        let has_audio = r.audio_path.as_ref()
            .map(|p| !p.is_empty()).unwrap_or(false);
        let text = r.rewritten
            .filter(|s| !s.is_empty())
            .or_else(|| r.corrected_transcribed.filter(|s| !s.is_empty()))
            .unwrap_or(r.transcribed);
        let ts = format_timestamp(r.created_at);
        Some(HistoryCardData {
            record_id: id,
            timestamp: ts,
            text,
            has_audio,
            is_llm_rewritten,
            was_pasted: false,
        })
    }).collect();
    Ok((cards, total))
}
```

在文件底部 `fn escape_fts5_query` 之前添加辅助函数：

```rust
fn format_timestamp(ts_ms: u64) -> String {
    use std::time::{Duration, UNIX_EPOCH};
    // ts 单位：毫秒
    let secs = ts_ms / 1000;
    let d = UNIX_EPOCH + Duration::from_secs(secs);
    let datetime: chrono::DateTime<chrono::Local> = d.into();
    datetime.format("%Y-%m-%d %H:%M").to_string()
}
```

- [ ] **Step 2: 在 `shanji-core/Cargo.toml` 添加 `chrono` 依赖**

```toml
chrono = { version = "0.4", features = ["clock"] }
```

- [ ] **Step 3: 编写测试**

在 `history.rs` 的 `#[cfg(test)] mod tests` 中添加：

```rust
#[test]
fn test_list_cards_empty() {
    let db = create_test_db();
    let (cards, total) = db.list_cards(20).unwrap();
    assert_eq!(cards.len(), 0);
    assert_eq!(total, 0);
}

#[test]
fn test_list_cards_prefers_rewritten() {
    let db = create_test_db();
    let record = HistoryRecord {
        id: None,
        created_at: 1000000000000, // 毫秒
        transcribed: "raw".to_string(),
        rewritten: Some("polished".to_string()),
        live_transcribed: None,
        corrected_transcribed: None,
        duration_ms: None,
        model_id: None,
        live_model_id: None,
        refine_model_id: None,
        refine_enabled: false,
        provider_id: None,
        audio_path: Some("/tmp/test.wav".to_string()),
    };
    db.insert(&record).unwrap();
    let (cards, total) = db.list_cards(20).unwrap();
    assert_eq!(total, 1);
    assert_eq!(cards[0].text, "polished");
    assert!(cards[0].has_audio);
    assert!(cards[0].is_llm_rewritten);
    assert!(!cards[0].was_pasted);
}

#[test]
fn test_list_cards_no_audio_when_path_empty() {
    let db = create_test_db();
    let record = HistoryRecord {
        id: None,
        created_at: 1000000000000,
        transcribed: "hello".to_string(),
        rewritten: None,
        live_transcribed: None,
        corrected_transcribed: None,
        duration_ms: None,
        model_id: None,
        live_model_id: None,
        refine_model_id: None,
        refine_enabled: false,
        provider_id: None,
        audio_path: Some("".to_string()),
    };
    db.insert(&record).unwrap();
    let (cards, _) = db.list_cards(20).unwrap();
    assert!(!cards[0].has_audio);
}
```

- [ ] **Step 4: 运行测试确认通过**

```bash
cargo test -p shanji-core -- history
```

Expected: 所有 history 测试通过，包括新增的三条。

- [ ] **Step 5: Commit**

```bash
git add crates/shanji-core/src/history.rs crates/shanji-core/Cargo.toml Cargo.lock
git commit -m "feat(core): 添加 HistoryCardData 和 list_cards 方法"
```

---

### Task 2: 在 app.rs 添加按 ID 操作历史的函数及 LLM 配置写入函数

**Files:**
- Modify: `crates/shanji-slint/src/app.rs`

先阅读 app.rs 中已有的 `refresh_history_window`、`paste_latest_history`、`delete_latest_history`、`clear_history` 函数（约 426–710 行），以及 config 相关的写入方式。

- [ ] **Step 1: 添加 `load_history_cards` 函数**

在 app.rs 的 `clear_history` 函数之后添加。注意：`resolve_app_paths()` 返回 `AppPaths`（非 `Result`），不使用 `?` 算子。

```rust
pub fn load_history_cards() -> Result<(Vec<shanji_core::history::HistoryCardData>, u32), String> {
    let paths = resolve_app_paths();
    let db = shanji_core::history::HistoryDb::new_with_paths(&paths)
        .map_err(|e| e.to_string())?;
    db.list_cards(20).map_err(|e| e.to_string())
}
```

- [ ] **Step 2: 添加按 ID 操作历史的函数**

`copy_to_clipboard` 是正确的函数名（见现有 `copy_latest_history` 的用法）。`open_path_in_system` 是 `app.rs` 内的私有辅助函数，直接调用。

```rust
pub fn copy_history_record(record_id: i32) -> Result<(), String> {
    let paths = resolve_app_paths();
    let db = shanji_core::history::HistoryDb::new_with_paths(&paths)
        .map_err(|e| e.to_string())?;
    let record = db.get(record_id as i64)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Record {} not found", record_id))?;
    let text = record_output_text(&record);
    shanji_core::output::copy_to_clipboard(&text).map_err(|e| e.to_string())
}

pub fn paste_history_record(record_id: i32) -> Result<(), String> {
    let paths = resolve_app_paths();
    let db = shanji_core::history::HistoryDb::new_with_paths(&paths)
        .map_err(|e| e.to_string())?;
    let record = db.get(record_id as i64)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Record {} not found", record_id))?;
    let cfg = config::get_config(&paths).map_err(|e| e.to_string())?;
    let text = record_output_text(&record);
    shanji_core::output::deliver_output(&text, &cfg.output).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn delete_history_record(record_id: i32) -> Result<(), String> {
    let paths = resolve_app_paths();
    let db = shanji_core::history::HistoryDb::new_with_paths(&paths)
        .map_err(|e| e.to_string())?;
    db.delete(record_id as i64).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn play_history_audio(record_id: i32) -> Result<(), String> {
    let paths = resolve_app_paths();
    let db = shanji_core::history::HistoryDb::new_with_paths(&paths)
        .map_err(|e| e.to_string())?;
    let record = db.get(record_id as i64)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Record {} not found", record_id))?;
    let audio_path = record.audio_path
        .ok_or_else(|| format!("Record {} has no audio file", record_id))?;
    open_path_in_system(&audio_path)
}

pub fn retranscribe_history(record_id: i32) -> Result<(), String> {
    // Phase 1：仅打开音频文件（完整重新转录需要音频管道，暂留为占位实现）
    play_history_audio(record_id)
}
```

- [ ] **Step 3: 添加 LLM 配置写入函数**

`RewriteConfig` 结构：`cfg.rewrite.providers: Vec<LlmProvider>`（每项有 `base_url`, `model`）；`cfg.rewrite.prompts: Vec<PromptPreset>`（每项有 `system_prompt`）；`cfg.rewrite.active_prompt_id: String` 指向当前激活的 prompt。

```rust
pub fn set_llm_base_url(url: String) -> Result<(), String> {
    let paths = resolve_app_paths();
    shanji_core::config::init_config(&paths).map_err(|e| e.to_string())?;
    let mut cfg = shanji_core::config::get_config(&paths).map_err(|e| e.to_string())?;
    if let Some(provider) = cfg.rewrite.providers.first_mut() {
        provider.base_url = url;
    }
    shanji_core::config::save_config(&paths, &cfg).map_err(|e| e.to_string())
}

pub fn set_llm_model_name(model: String) -> Result<(), String> {
    let paths = resolve_app_paths();
    shanji_core::config::init_config(&paths).map_err(|e| e.to_string())?;
    let mut cfg = shanji_core::config::get_config(&paths).map_err(|e| e.to_string())?;
    if let Some(provider) = cfg.rewrite.providers.first_mut() {
        provider.model = model;
    }
    shanji_core::config::save_config(&paths, &cfg).map_err(|e| e.to_string())
}

pub fn set_llm_api_key(key: String) -> Result<(), String> {
    let entry = keyring::Entry::new("shanji", "llm-api-key")
        .map_err(|e| e.to_string())?;
    entry.set_password(&key).map_err(|e| e.to_string())
}

pub fn set_llm_system_prompt(prompt: String) -> Result<(), String> {
    let paths = resolve_app_paths();
    shanji_core::config::init_config(&paths).map_err(|e| e.to_string())?;
    let mut cfg = shanji_core::config::get_config(&paths).map_err(|e| e.to_string())?;
    // system_prompt 存在于 prompts 列表中，修改当前激活的 prompt preset
    let active_id = cfg.rewrite.active_prompt_id.clone();
    if let Some(preset) = cfg.rewrite.prompts.iter_mut()
        .find(|p| p.id == active_id && !p.is_builtin)
    {
        preset.system_prompt = prompt;
    } else {
        // 如果没有自定义 preset，创建一个
        cfg.rewrite.prompts.push(shanji_core::config::PromptPreset {
            id: "custom".to_string(),
            name: "自定义".to_string(),
            system_prompt: prompt,
            is_builtin: false,
        });
        cfg.rewrite.active_prompt_id = "custom".to_string();
    }
    shanji_core::config::save_config(&paths, &cfg).map_err(|e| e.to_string())
}
```

- [ ] **Step 4: 确认编译通过**

```bash
cargo check -p shanji-app
```

Expected: 无 error（可有 warning）

- [ ] **Step 5: Commit**

```bash
git add crates/shanji-slint/src/app.rs
git commit -m "feat(app): 添加按 ID 历史操作和 LLM 配置写入函数"
```

---

## Chunk 2: Slint UI — 新 settings-window.slint

### Task 3: 重写 settings-window.slint — 框架 + Palette + 侧边栏

**Files:**
- Modify: `crates/shanji-slint/ui/settings-window.slint`

这是最大的单个任务。分三步完成：先写骨架，再写各页面。

- [ ] **Step 1: 替换整个文件为新框架（Palette + enum + 窗口骨架 + 侧边栏）**

```slint
import { Button, ScrollView, LineEdit, TextEdit, CheckBox } from "std-widgets.slint";

// ── 配色 Palette（浅色模式，in-out 支持将来深色模式切换）─────────────────
global Palette {
    in-out property <brush> sidebar-bg: #f3f4f6;
    in-out property <brush> content-bg: #ffffff;
    in-out property <brush> card-bg: #f9fafb;
    in-out property <brush> border: #e5e7eb;
    in-out property <brush> text-primary: #111827;
    in-out property <brush> text-secondary: #6b7280;
    in-out property <brush> text-muted: #9ca3af;
    in-out property <brush> nav-active-bg: #e5e7eb;
    in-out property <brush> accent: #2563eb;
    in-out property <brush> danger: #dc2626;
    in-out property <brush> danger-bg: #fef2f2;
    in-out property <brush> danger-border: #fecaca;
}

// ── 页面枚举 ──────────────────────────────────────────────────────────────
enum SettingsSection { Asr, Llm, Output, Hotkeys, Network, History, About }

// ── 历史卡片数据结构 ───────────────────────────────────────────────────────
struct HistoryCardData {
    record-id: int,
    timestamp: string,
    text: string,
    has-audio: bool,
    is-llm-rewritten: bool,
    was-pasted: bool,
}

// ── 可复用：侧边栏菜单项 ──────────────────────────────────────────────────
component NavItem inherits Rectangle {
    in property <string> label;
    in property <bool> active: false;
    callback clicked;

    height: 34px;
    border-radius: 6px;
    background: active ? Palette.nav-active-bg : transparent;

    ta := TouchArea {
        clicked => { root.clicked(); }
        mouse-cursor: pointer;
    }

    HorizontalLayout {
        padding-left: 12px;
        padding-right: 12px;
        alignment: start;
        spacing: 8px;

        Text {
            text: root.label;
            color: root.active ? Palette.text-primary : Palette.text-secondary;
            font-size: 13px;
            font-weight: root.active ? 600 : 400;
            vertical-alignment: center;
        }
    }
}

// ── 可复用：设置分组标题 ──────────────────────────────────────────────────
component SectionLabel inherits Text {
    in property <string> label;
    text: root.label;
    color: Palette.text-muted;
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.6px;
}

// ── 可复用：设置行（label 在左，@children 在右）───────────────────────────
component SettingRow inherits Rectangle {
    in property <string> label;
    in property <string> description: "";
    in property <bool> enabled: true;

    height: description == "" ? 44px : 54px;

    HorizontalLayout {
        padding-left: 14px;
        padding-right: 14px;
        alignment: space-between;

        VerticalLayout {
            alignment: center;
            spacing: 2px;
            Text {
                text: root.label;
                color: root.enabled ? Palette.text-primary : Palette.text-muted;
                font-size: 12px;
                font-weight: 600;
            }
            if root.description != "": Text {
                text: root.description;
                color: Palette.text-muted;
                font-size: 10px;
            }
        }

        // 右侧 slot：调用方放子控件
        @children
    }
}

// ── 可复用：开关控件 ───────────────────────────────────────────────────────
component ToggleSwitch inherits Rectangle {
    in-out property <bool> checked: false;
    in property <bool> enabled: true;
    callback toggled(bool);

    width: 36px;
    height: 20px;
    border-radius: 10px;
    background: root.checked ? (root.enabled ? Palette.accent : #93c5fd) : #d1d5db;

    ta := TouchArea {
        enabled: root.enabled;
        clicked => {
            root.checked = !root.checked;
            root.toggled(root.checked);
        }
        mouse-cursor: pointer;
    }

    thumb := Rectangle {
        x: root.checked ? (parent.width - self.width - 2px) : 2px;
        y: 2px;
        width: 16px;
        height: 16px;
        border-radius: 8px;
        background: #ffffff;
        drop-shadow-blur: 2px;
        drop-shadow-color: rgba(0,0,0,0.15);

        animate x { duration: 120ms; easing: ease-in-out; }
    }
}

// ── 主窗口 ─────────────────────────────────────────────────────────────────
export component SettingsWindow inherits Window {
    // ── 导航状态 ──
    in-out property <SettingsSection> active-section: SettingsSection.Asr;

    // ── ASR 页属性 ──
    in-out property <string> model-text: "paraformer-zh-streaming";
    in-out property <string> audio-device-text: "系统默认";
    in-out property <string> recording-mode-text: "toggle";
    in-out property <bool>   refine-asr-enabled: false;
    in-out property <string> hotword-summary-text: "无热词库";

    // ── LLM 页属性 ──
    in-out property <bool>   llm-enabled: false;
    in-out property <string> llm-base-url: "https://api.openai.com/v1";
    in-out property <string> llm-model-name: "gpt-4o-mini";
    in-out property <string> llm-system-prompt: "";

    // ── Output 页属性 ──
    in-out property <string> punct-style-text: "zh";
    in-out property <string> append-content-text: "space";
    in-out property <bool>   overlay-visible: false;
    in-out property <string> theme-text: "system";

    // ── Hotkeys 页属性 ──
    in-out property <string> hotkey-summary-text: "未设置";

    // ── Network 页属性 ──
    in-out property <int>    github-proxy-index: 0;

    // ── History 页属性 ──
    in property  <[HistoryCardData]> history-records: [];
    in-out property <string> history-stats-text: "共 0 条";

    // ── About 页属性 ──
    in-out property <string> app-version-text: "v0.1";
    in-out property <string> config-path-text: "";

    // ── Callbacks ──
    // ASR
    callback cycle-live-model-requested();
    callback cycle-refine-model-requested();
    callback toggle-refine-asr-requested();
    callback download-live-model-requested();
    callback download-refine-model-requested();
    callback cycle-audio-device-requested();
    callback cycle-recording-mode-requested();
    callback toggle-hotword-requested();
    // LLM
    callback toggle-rewrite-requested();
    callback set-llm-base-url(string);
    callback set-llm-api-key(string);
    callback set-llm-model-name(string);
    callback set-llm-system-prompt(string);
    // Output
    callback cycle-punct-style-requested();
    callback cycle-append-content-requested();
    callback toggle-overlay-requested();
    callback cycle-theme-requested();
    // Hotkeys
    callback record-hotkey-requested();
    // Network
    callback cycle-github-proxy-requested();
    // History
    callback refresh-history-requested();
    callback clear-history-requested();
    callback play-audio-requested(int);
    callback retranscribe-requested(int);
    callback copy-record-requested(int);
    callback paste-record-requested(int);
    callback delete-record-requested(int);
    // About
    callback clear-session-requested();

    title: "闪记";
    width: 900px;
    height: 620px;
    default-font-family: "Hiragino Sans GB";

    Rectangle {
        background: Palette.content-bg;

        HorizontalLayout {

            // ── 左侧边栏 ─────────────────────────────────────────────────
            Rectangle {
                width: 180px;
                background: Palette.sidebar-bg;
                border-width: 0px;

                // 右侧 1px 分割线
                Rectangle {
                    x: parent.width - 1px;
                    width: 1px;
                    height: parent.height;
                    background: Palette.border;
                }

                VerticalLayout {
                    padding: 0px;

                    // App 信息区
                    Rectangle {
                        height: 56px;
                        VerticalLayout {
                            padding-left: 14px;
                            padding-right: 14px;
                            alignment: center;
                            spacing: 2px;
                            Text {
                                text: "闪记";
                                color: Palette.text-primary;
                                font-size: 15px;
                                font-weight: 800;
                            }
                            Text {
                                text: root.app-version-text + " · 设置";
                                color: Palette.text-muted;
                                font-size: 11px;
                            }
                        }
                        // 底部分割线
                        Rectangle {
                            y: parent.height - 1px;
                            height: 1px;
                            background: Palette.border;
                        }
                    }

                    // 菜单项
                    Rectangle { height: 6px; }  // 顶部间距

                    Rectangle {
                        height: 34px;
                        margin: 0px 8px;
                        NavItem {
                            width: 100%;
                            label: "🎙 语音识别";
                            active: root.active-section == SettingsSection.Asr;
                            clicked => { root.active-section = SettingsSection.Asr; }
                        }
                    }
                    Rectangle {
                        height: 34px;
                        margin: 0px 8px;
                        NavItem {
                            width: 100%;
                            label: "✨ LLM 润色";
                            active: root.active-section == SettingsSection.Llm;
                            clicked => { root.active-section = SettingsSection.Llm; }
                        }
                    }
                    Rectangle {
                        height: 34px;
                        margin: 0px 8px;
                        NavItem {
                            width: 100%;
                            label: "⌨️ 输出";
                            active: root.active-section == SettingsSection.Output;
                            clicked => { root.active-section = SettingsSection.Output; }
                        }
                    }
                    Rectangle {
                        height: 34px;
                        margin: 0px 8px;
                        NavItem {
                            width: 100%;
                            label: "🔑 快捷键";
                            active: root.active-section == SettingsSection.Hotkeys;
                            clicked => { root.active-section = SettingsSection.Hotkeys; }
                        }
                    }
                    Rectangle {
                        height: 34px;
                        margin: 0px 8px;
                        NavItem {
                            width: 100%;
                            label: "🌐 网络";
                            active: root.active-section == SettingsSection.Network;
                            clicked => { root.active-section = SettingsSection.Network; }
                        }
                    }
                    Rectangle {
                        height: 34px;
                        margin: 0px 8px;
                        NavItem {
                            width: 100%;
                            label: "🕑 历史记录";
                            active: root.active-section == SettingsSection.History;
                            clicked => { root.active-section = SettingsSection.History; }
                        }
                    }

                    // 弹性空白
                    Rectangle { vertical-stretch: 1; }

                    // 关于（置底）
                    Rectangle {
                        height: 1px;
                        background: Palette.border;
                    }
                    Rectangle {
                        height: 34px;
                        margin: 0px 8px;
                        NavItem {
                            width: 100%;
                            label: "ℹ️ 关于";
                            active: root.active-section == SettingsSection.About;
                            clicked => { root.active-section = SettingsSection.About; }
                        }
                    }
                    Rectangle { height: 8px; }
                }
            }

            // ── 右侧内容区（条件渲染，下一步填充各页面）─────────────────
            Rectangle {
                horizontal-stretch: 1;
                background: Palette.content-bg;
                clip: true;

                // 各页面在下一个任务中填充
                Text {
                    text: "Loading...";
                    color: Palette.text-muted;
                    horizontal-alignment: center;
                    vertical-alignment: center;
                }
            }
        }
    }
}
```

- [ ] **Step 2: 确认 Slint 编译通过**

```bash
cargo check -p shanji-app 2>&1 | head -40
```

Expected: 无 Slint 解析错误（可有 Rust 未使用变量 warning）

- [ ] **Step 3: Commit 骨架**

```bash
git add crates/shanji-slint/ui/settings-window.slint
git commit -m "feat(ui): settings-window 新骨架 — Palette + SettingsSection enum + 侧边栏"
```

---

### Task 4: 填充 settings-window.slint 右侧各页面

**Files:**
- Modify: `crates/shanji-slint/ui/settings-window.slint`

将 Task 3 中右侧内容区的占位 `Text` 替换为真实的条件渲染页面。

- [ ] **Step 1: 替换右侧内容区，添加 AsrPage、LlmPage、OutputPage、HotkeysPage、NetworkPage**

将右侧 `Rectangle` 中的占位 `Text` 替换为：

```slint
// ── 语音识别页 ──
if root.active-section == SettingsSection.Asr: ScrollView {
    width: 100%;
    height: 100%;
    VerticalLayout {
        padding: 20px;
        spacing: 16px;

        // 页头
        VerticalLayout {
            spacing: 4px;
            Text { text: "语音识别"; color: Palette.text-primary; font-size: 16px; font-weight: 700; }
            Text { text: "ASR 模型、麦克风、录音行为"; color: Palette.text-muted; font-size: 11px; }
        }

        // 模型 小节
        SectionLabel { label: "模型"; }
        Rectangle {
            border-radius: 8px;
            background: Palette.card-bg;
            border-width: 1px;
            border-color: Palette.border;
            VerticalLayout {
                // 实时模型行
                Rectangle {
                    height: 54px;
                    border-bottom-width: 1px;
                    border-bottom-color: Palette.border;
                    HorizontalLayout {
                        padding-left: 14px; padding-right: 14px;
                        alignment: space-between;
                        VerticalLayout {
                            alignment: center; spacing: 2px;
                            Text { text: "实时模型"; color: Palette.text-primary; font-size: 12px; font-weight: 600; }
                            Text { text: root.model-text; color: Palette.text-muted; font-size: 10px; }
                        }
                        Button { text: "切换"; clicked => { root.cycle-live-model-requested(); } }
                    }
                }
                // 整体纠正行
                Rectangle {
                    height: 54px;
                    HorizontalLayout {
                        padding-left: 14px; padding-right: 14px;
                        alignment: space-between;
                        VerticalLayout {
                            alignment: center; spacing: 2px;
                            Text { text: "整体纠正"; color: Palette.text-primary; font-size: 12px; font-weight: 600; }
                            Text { text: "录音结束后二次修正全文"; color: Palette.text-muted; font-size: 10px; }
                        }
                        ToggleSwitch {
                            checked: root.refine-asr-enabled;
                            toggled(v) => { root.toggle-refine-asr-requested(); }
                        }
                    }
                }
            }
        }

        // 音频 小节
        SectionLabel { label: "音频"; }
        Rectangle {
            border-radius: 8px;
            background: Palette.card-bg;
            border-width: 1px;
            border-color: Palette.border;
            VerticalLayout {
                Rectangle {
                    height: 44px;
                    border-bottom-width: 1px;
                    border-bottom-color: Palette.border;
                    HorizontalLayout {
                        padding-left: 14px; padding-right: 14px;
                        alignment: space-between;
                        Text { text: "输入设备"; color: Palette.text-primary; font-size: 12px; font-weight: 600; vertical-alignment: center; }
                        Button { text: root.audio-device-text; clicked => { root.cycle-audio-device-requested(); } }
                    }
                }
                Rectangle {
                    height: 44px;
                    HorizontalLayout {
                        padding-left: 14px; padding-right: 14px;
                        alignment: space-between;
                        Text { text: "录音模式"; color: Palette.text-primary; font-size: 12px; font-weight: 600; vertical-alignment: center; }
                        Button { text: root.recording-mode-text; clicked => { root.cycle-recording-mode-requested(); } }
                    }
                }
            }
        }

        // 热词库 小节
        SectionLabel { label: "热词库"; }
        Rectangle {
            border-radius: 8px;
            background: Palette.card-bg;
            border-width: 1px;
            border-color: Palette.border;
            height: 44px;
            HorizontalLayout {
                padding-left: 14px; padding-right: 14px;
                alignment: space-between;
                VerticalLayout {
                    alignment: center;
                    Text { text: root.hotword-summary-text; color: Palette.text-secondary; font-size: 11px; }
                }
                Button { text: "切换"; clicked => { root.toggle-hotword-requested(); } }
            }
        }

        // 模型管理 小节
        SectionLabel { label: "模型管理"; }
        HorizontalLayout {
            spacing: 8px;
            Button { text: "下载实时模型"; clicked => { root.download-live-model-requested(); } }
            Button { text: "下载纠正模型"; clicked => { root.download-refine-model-requested(); } }
        }
    }
}

// ── LLM 润色页 ──
if root.active-section == SettingsSection.Llm: ScrollView {
    width: 100%;
    height: 100%;
    VerticalLayout {
        padding: 20px;
        spacing: 16px;

        VerticalLayout {
            spacing: 4px;
            Text { text: "LLM 润色"; color: Palette.text-primary; font-size: 16px; font-weight: 700; }
            Text { text: "通过大模型对转录文本进行改写"; color: Palette.text-muted; font-size: 11px; }
        }

        // 启用开关
        Rectangle {
            border-radius: 8px;
            background: Palette.card-bg;
            border-width: 1px;
            border-color: Palette.border;
            height: 54px;
            HorizontalLayout {
                padding-left: 14px; padding-right: 14px;
                alignment: space-between;
                VerticalLayout {
                    alignment: center; spacing: 2px;
                    Text { text: "启用 LLM 润色"; color: Palette.text-primary; font-size: 12px; font-weight: 600; }
                    Text { text: "关闭时直接输出 ASR 原始结果"; color: Palette.text-muted; font-size: 10px; }
                }
                ToggleSwitch {
                    checked: root.llm-enabled;
                    toggled(v) => { root.toggle-rewrite-requested(); }
                }
            }
        }

        // API 配置 小节
        SectionLabel { label: "API 配置"; }
        Rectangle {
            border-radius: 8px;
            background: Palette.card-bg;
            border-width: 1px;
            border-color: Palette.border;
            VerticalLayout {
                padding: 12px 14px;
                spacing: 10px;

                VerticalLayout {
                    spacing: 4px;
                    Text { text: "API Base URL"; color: root.llm-enabled ? Palette.text-primary : Palette.text-muted; font-size: 11px; font-weight: 600; }
                    LineEdit {
                        text: root.llm-base-url;
                        enabled: root.llm-enabled;
                        edited(v) => { root.set-llm-base-url(v); }
                    }
                }
                VerticalLayout {
                    spacing: 4px;
                    HorizontalLayout {
                        alignment: space-between;
                        Text { text: "API Key"; color: root.llm-enabled ? Palette.text-primary : Palette.text-muted; font-size: 11px; font-weight: 600; }
                        Text { text: "存储在系统钥匙串"; color: Palette.text-muted; font-size: 9px; vertical-alignment: center; }
                    }
                    LineEdit {
                        input-type: password;
                        placeholder-text: "sk-...";
                        enabled: root.llm-enabled;
                        accepted(v) => { root.set-llm-api-key(v); }
                    }
                }
                VerticalLayout {
                    spacing: 4px;
                    Text { text: "模型名称"; color: root.llm-enabled ? Palette.text-primary : Palette.text-muted; font-size: 11px; font-weight: 600; }
                    LineEdit {
                        text: root.llm-model-name;
                        enabled: root.llm-enabled;
                        edited(v) => { root.set-llm-model-name(v); }
                    }
                }
            }
        }

        // 提示词 小节
        SectionLabel { label: "润色提示词"; }
        Rectangle {
            border-radius: 8px;
            background: Palette.card-bg;
            border-width: 1px;
            border-color: Palette.border;
            height: 120px;
            TextEdit {
                text: root.llm-system-prompt;
                enabled: root.llm-enabled;
                edited(v) => { root.set-llm-system-prompt(v); }
            }
        }
    }
}

// ── 输出页 ──
if root.active-section == SettingsSection.Output: ScrollView {
    width: 100%;
    height: 100%;
    VerticalLayout {
        padding: 20px;
        spacing: 16px;

        VerticalLayout {
            spacing: 4px;
            Text { text: "输出"; color: Palette.text-primary; font-size: 16px; font-weight: 700; }
            Text { text: "文本格式与粘贴行为"; color: Palette.text-muted; font-size: 11px; }
        }

        SectionLabel { label: "格式"; }
        Rectangle {
            border-radius: 8px;
            background: Palette.card-bg;
            border-width: 1px;
            border-color: Palette.border;
            VerticalLayout {
                Rectangle {
                    height: 44px;
                    border-bottom-width: 1px;
                    border-bottom-color: Palette.border;
                    HorizontalLayout {
                        padding-left: 14px; padding-right: 14px;
                        alignment: space-between;
                        Text { text: "标点风格"; color: Palette.text-primary; font-size: 12px; font-weight: 600; vertical-alignment: center; }
                        Button { text: root.punct-style-text; clicked => { root.cycle-punct-style-requested(); } }
                    }
                }
                Rectangle {
                    height: 44px;
                    HorizontalLayout {
                        padding-left: 14px; padding-right: 14px;
                        alignment: space-between;
                        Text { text: "追加内容"; color: Palette.text-primary; font-size: 12px; font-weight: 600; vertical-alignment: center; }
                        Button { text: root.append-content-text; clicked => { root.cycle-append-content-requested(); } }
                    }
                }
            }
        }

        SectionLabel { label: "界面"; }
        Rectangle {
            border-radius: 8px;
            background: Palette.card-bg;
            border-width: 1px;
            border-color: Palette.border;
            height: 44px;
            HorizontalLayout {
                padding-left: 14px; padding-right: 14px;
                alignment: space-between;
                Text { text: "悬浮窗"; color: Palette.text-primary; font-size: 12px; font-weight: 600; vertical-alignment: center; }
                ToggleSwitch {
                    checked: root.overlay-visible;
                    toggled(v) => { root.toggle-overlay-requested(); }
                }
            }
        }

        SectionLabel { label: "外观"; }
        Rectangle {
            border-radius: 8px;
            background: Palette.card-bg;
            border-width: 1px;
            border-color: Palette.border;
            height: 44px;
            HorizontalLayout {
                padding-left: 14px; padding-right: 14px;
                alignment: space-between;
                Text { text: "主题"; color: Palette.text-primary; font-size: 12px; font-weight: 600; vertical-alignment: center; }
                Button { text: root.theme-text; clicked => { root.cycle-theme-requested(); } }
            }
        }
    }
}

// ── 快捷键页 ──
if root.active-section == SettingsSection.Hotkeys: ScrollView {
    width: 100%;
    height: 100%;
    VerticalLayout {
        padding: 20px;
        spacing: 16px;

        VerticalLayout {
            spacing: 4px;
            Text { text: "快捷键"; color: Palette.text-primary; font-size: 16px; font-weight: 700; }
            Text { text: "全局快捷键设置"; color: Palette.text-muted; font-size: 11px; }
        }

        SectionLabel { label: "PUSH-TO-TALK"; }
        Rectangle {
            border-radius: 8px;
            background: Palette.card-bg;
            border-width: 1px;
            border-color: Palette.border;
            height: 54px;
            HorizontalLayout {
                padding-left: 14px; padding-right: 14px;
                alignment: space-between;
                VerticalLayout {
                    alignment: center; spacing: 2px;
                    Text { text: "当前快捷键"; color: Palette.text-primary; font-size: 12px; font-weight: 600; }
                    Text { text: root.hotkey-summary-text; color: Palette.text-muted; font-size: 10px; }
                }
                Button { text: "重新录制"; clicked => { root.record-hotkey-requested(); } }
            }
        }
    }
}

// ── 网络页 ──
if root.active-section == SettingsSection.Network: ScrollView {
    width: 100%;
    height: 100%;
    VerticalLayout {
        padding: 20px;
        spacing: 16px;

        VerticalLayout {
            spacing: 4px;
            Text { text: "网络"; color: Palette.text-primary; font-size: 16px; font-weight: 700; }
            Text { text: "模型下载代理配置"; color: Palette.text-muted; font-size: 11px; }
        }

        SectionLabel { label: "下载代理"; }
        Rectangle {
            border-radius: 8px;
            background: Palette.card-bg;
            border-width: 1px;
            border-color: Palette.border;
            height: 44px;
            HorizontalLayout {
                padding-left: 14px; padding-right: 14px;
                alignment: space-between;
                Text { text: "GitHub 代理"; color: Palette.text-primary; font-size: 12px; font-weight: 600; vertical-alignment: center; }
                Button { text: "切换"; clicked => { root.cycle-github-proxy-requested(); } }
            }
        }
    }
}

// ── 历史记录页 ──
if root.active-section == SettingsSection.History: Rectangle {
    VerticalLayout {
        // 顶部操作栏（固定）
        Rectangle {
            height: 52px;
            border-bottom-width: 1px;
            border-bottom-color: Palette.border;
            HorizontalLayout {
                padding-left: 20px; padding-right: 20px;
                alignment: space-between;
                VerticalLayout {
                    alignment: center;
                    Text { text: "历史记录"; color: Palette.text-primary; font-size: 16px; font-weight: 700; }
                    Text { text: root.history-stats-text; color: Palette.text-muted; font-size: 11px; }
                }
                HorizontalLayout {
                    alignment: end;
                    spacing: 8px;
                    Button { text: "刷新"; clicked => { root.refresh-history-requested(); } }
                    Button { text: "清空全部"; clicked => { root.clear-history-requested(); } }
                }
            }
        }

        // 卡片列表（可滚动）
        ScrollView {
            vertical-stretch: 1;
            VerticalLayout {
                padding: 16px;
                spacing: 10px;

                for card in root.history-records: Rectangle {
                    border-radius: 8px;
                    background: Palette.card-bg;
                    border-width: 1px;
                    border-color: Palette.border;
                    height: card-layout.preferred-height + 24px;

                    card-layout := VerticalLayout {
                        padding: 12px 14px;
                        spacing: 8px;

                        // 时间戳 + 操作按钮
                        HorizontalLayout {
                            alignment: space-between;
                            Text {
                                text: card.timestamp;
                                color: Palette.text-muted;
                                font-size: 10px;
                                vertical-alignment: center;
                            }
                            HorizontalLayout {
                                spacing: 4px;
                                if card.has-audio: Button {
                                    text: "▶ 播放";
                                    clicked => { root.play-audio-requested(card.record-id); }
                                }
                                if !card.has-audio: Rectangle {
                                    width: 0px;  // 占位，不显示
                                }
                                if card.has-audio: Button {
                                    text: "↻ 重新转录";
                                    clicked => { root.retranscribe-requested(card.record-id); }
                                }
                                if !card.has-audio: Rectangle { width: 0px; }
                                Button { text: "复制"; clicked => { root.copy-record-requested(card.record-id); } }
                                Button { text: "粘贴"; clicked => { root.paste-record-requested(card.record-id); } }
                                Button { text: "删除"; clicked => { root.delete-record-requested(card.record-id); } }
                            }
                        }

                        // 正文
                        Text {
                            text: card.text;
                            color: Palette.text-primary;
                            font-size: 12px;
                            wrap: word-wrap;
                        }

                        // 底部标签
                        HorizontalLayout {
                            spacing: 6px;
                            if card.is-llm-rewritten: Rectangle {
                                border-radius: 3px;
                                background: #eff6ff;
                                height: 16px;
                                width: tag-llm.preferred-width + 10px;
                                tag-llm := Text { text: "LLM润色"; color: #3b82f6; font-size: 9px; horizontal-alignment: center; vertical-alignment: center; }
                            }
                            if !card.is-llm-rewritten: Rectangle {
                                border-radius: 3px;
                                background: #f3f4f6;
                                height: 16px;
                                width: tag-raw.preferred-width + 10px;
                                tag-raw := Text { text: "原始转录"; color: Palette.text-muted; font-size: 9px; horizontal-alignment: center; vertical-alignment: center; }
                            }
                            if card.has-audio: Rectangle {
                                border-radius: 3px;
                                background: Palette.card-bg;
                                border-width: 1px;
                                border-color: Palette.border;
                                height: 16px;
                                width: tag-audio.preferred-width + 10px;
                                tag-audio := Text { text: "🎙 有录音"; color: Palette.text-muted; font-size: 9px; horizontal-alignment: center; vertical-alignment: center; }
                            }
                        }
                    }
                }
            }
        }
    }
}

// ── 关于页 ──
if root.active-section == SettingsSection.About: ScrollView {
    width: 100%;
    height: 100%;
    VerticalLayout {
        padding: 20px;
        spacing: 16px;

        VerticalLayout {
            spacing: 4px;
            Text { text: "关于"; color: Palette.text-primary; font-size: 16px; font-weight: 700; }
            Text { text: "闪记 · 本地 AI 语音输入"; color: Palette.text-muted; font-size: 11px; }
        }

        Rectangle {
            border-radius: 8px;
            background: Palette.card-bg;
            border-width: 1px;
            border-color: Palette.border;
            VerticalLayout {
                Rectangle {
                    height: 44px;
                    border-bottom-width: 1px;
                    border-bottom-color: Palette.border;
                    HorizontalLayout {
                        padding-left: 14px; padding-right: 14px;
                        alignment: space-between;
                        Text { text: "版本"; color: Palette.text-primary; font-size: 12px; font-weight: 600; vertical-alignment: center; }
                        Text { text: root.app-version-text; color: Palette.text-muted; font-size: 12px; vertical-alignment: center; }
                    }
                }
                Rectangle {
                    height: 54px;
                    HorizontalLayout {
                        padding-left: 14px; padding-right: 14px;
                        alignment: space-between;
                        VerticalLayout {
                            alignment: center; spacing: 2px;
                            Text { text: "配置路径"; color: Palette.text-primary; font-size: 12px; font-weight: 600; }
                            Text { text: root.config-path-text; color: Palette.text-muted; font-size: 10px; wrap: word-wrap; }
                        }
                    }
                }
            }
        }

        Rectangle {
            border-radius: 8px;
            background: Palette.danger-bg;
            border-width: 1px;
            border-color: Palette.danger-border;
            height: 44px;
            HorizontalLayout {
                padding-left: 14px; padding-right: 14px;
                alignment: space-between;
                Text { text: "清理会话"; color: Palette.danger; font-size: 12px; font-weight: 600; vertical-alignment: center; }
                Button { text: "清理"; clicked => { root.clear-session-requested(); } }
            }
        }
    }
}
```

- [ ] **Step 2: 确认编译通过**

```bash
cargo check -p shanji-app 2>&1 | head -60
```

Expected: 无 error

- [ ] **Step 3: Commit**

```bash
git add crates/shanji-slint/ui/settings-window.slint
git commit -m "feat(ui): settings-window 填充全部 7 个页面内容"
```

---

## Chunk 3: main.rs 重构 — 移除 HistoryWindow，连接新接口

### Task 5: 重构 main.rs — 移除 HistoryWindow，连接新 SettingsWindow callbacks

**Files:**
- Modify: `crates/shanji-slint/src/main.rs`

**实施顺序说明**：Step 4 定义了 `refresh_settings_from_app` 辅助函数，Step 2/3 依赖它。实际实施时先写 Step 4，再写 Step 1-3，或在 Step 2/3 中写 forward declaration 注释并在 Step 4 填充函数体。以下步骤顺序方便理解，执行时可先跳到 Step 4 写好函数，再回来改事件绑定。

- [ ] **Step 1: 删除 `HistoryWindow` 相关代码**

从 `main()` 函数中删除以下内容：
1. `let history = HistoryWindow::new()?;`
2. `apply_history_snapshot(&history, app::refresh_history_window());`
3. `app.on_open_history_requested(...)` 闭包（约 71–77 行）— Step 2 将重新添加
4. `history.on_refresh_requested(...)` 等全部 6 个 history callback 绑定（约 123–164 行）
5. `platform_timer` 和 `refresh_timer` 闭包中的 `weak_history` 及相关 `history.upgrade()` 引用
6. `apply_history_snapshot` 函数（约 628–642 行）

- [ ] **Step 2: 修改 `app.on_open_history_requested`，改为打开 settings 并切换到 History 页**

`SettingsSection` 是 Slint 生成的 Rust 枚举，名称与 `.slint` 文件中的 `enum SettingsSection` 一致。

```rust
let weak_settings = settings.as_weak();
app.on_open_history_requested(move || {
    if let Some(settings) = weak_settings.upgrade() {
        settings.set_active_section(SettingsSection::History);
        let _ = settings.show();
        refresh_settings_from_app(&settings);
    }
});
```

- [ ] **Step 3: 修改 `handle_platform_hotkey_event` 和 `handle_platform_tray_event`**

移除 `&HistoryWindow` 参数，将 `OpenHistory` 分支改为：

```rust
(HotkeyAction::OpenHistory, HotkeyEventState::Pressed) => {
    settings.set_active_section(SettingsSection::History);
    let _ = settings.show();
    refresh_settings_from_app(settings);
}
```

```rust
PlatformTrayEvent::Action(TrayAction::OpenHistory) => {
    settings.set_active_section(SettingsSection::History);
    let _ = settings.show();
    refresh_settings_from_app(settings);
}
```

同时更新 `TrayAction::Quit` 分支：移除 `let _ = history.hide();`

- [ ] **Step 4: 新增 `refresh_settings_from_app` 辅助函数**

将分散的 snapshot 刷新逻辑抽取为：

```rust
fn refresh_settings_from_app(settings: &SettingsWindow) {
    if let Ok(snapshot) = app::refresh_settings_window() {
        apply_settings_snapshot(settings, Ok(snapshot));
    }
    // 历史卡片
    // 注意：HistoryCardData 此处是 Slint 生成的 Rust struct（字段类型为 SharedString 等），
    //       与 shanji_core::history::HistoryCardData 不同
    match app::load_history_cards() {
        Ok((cards, total)) => {
            let slint_cards: Vec<HistoryCardData> = cards.into_iter().map(|c| HistoryCardData {
                record_id: c.record_id,
                timestamp: c.timestamp.into(),
                text: c.text.into(),
                has_audio: c.has_audio,
                is_llm_rewritten: c.is_llm_rewritten,
                was_pasted: c.was_pasted,
            }).collect();
            settings.set_history_records(
                std::rc::Rc::new(slint::VecModel::from(slint_cards)).into()
            );
            settings.set_history_stats_text(
                format!("共 {} 条 · 最近 20 条", total).into()
            );
        }
        Err(e) => log::warn!("Failed to load history cards: {}", e),
    }
}

- [ ] **Step 5: 绑定新 history callbacks 到 `settings`**

在现有 settings callbacks 之后添加：

```rust
let weak_settings = settings.as_weak();
settings.on_refresh_history_requested(move || {
    if let Some(settings) = weak_settings.upgrade() {
        refresh_settings_from_app(&settings);
    }
});

let weak_settings = settings.as_weak();
settings.on_clear_history_requested(move || {
    if let Some(settings) = weak_settings.upgrade() {
        let _ = app::clear_history();
        refresh_settings_from_app(&settings);
    }
});

settings.on_copy_record_requested(move |id| {
    let _ = app::copy_history_record(id);
});

let weak_settings = settings.as_weak();
settings.on_paste_record_requested(move |id| {
    let _ = app::paste_history_record(id);
    if let Some(settings) = weak_settings.upgrade() {
        refresh_settings_from_app(&settings);
    }
});

let weak_settings = settings.as_weak();
settings.on_delete_record_requested(move |id| {
    if let Some(settings) = weak_settings.upgrade() {
        let _ = app::delete_history_record(id);
        refresh_settings_from_app(&settings);
    }
});

settings.on_play_audio_requested(move |id| {
    let _ = app::play_history_audio(id);
});

settings.on_retranscribe_requested(move |id| {
    let _ = app::retranscribe_history(id);
});
```

- [ ] **Step 6: 绑定 LLM 配置写入 callbacks**

```rust
settings.on_set_llm_base_url(move |url| {
    let _ = app::set_llm_base_url(url.to_string());
});

settings.on_set_llm_api_key(move |key| {
    let _ = app::set_llm_api_key(key.to_string());
});

settings.on_set_llm_model_name(move |model| {
    let _ = app::set_llm_model_name(model.to_string());
});

settings.on_set_llm_system_prompt(move |prompt| {
    let _ = app::set_llm_system_prompt(prompt.to_string());
});
```

- [ ] **Step 7: 更新 `apply_settings_snapshot` 以填充新属性**

修改 `apply_settings_snapshot`，在原有 set_* 调用基础上添加：

```rust
settings.set_llm_enabled(snapshot.llm_enabled);
settings.set_llm_base_url(snapshot.llm_base_url.into());
settings.set_llm_model_name(snapshot.llm_model_name.into());
settings.set_llm_system_prompt(snapshot.llm_system_prompt.into());
settings.set_refine_asr_enabled(snapshot.refine_asr_enabled);
settings.set_overlay_visible(snapshot.overlay_enabled);
settings.set_config_path_text(snapshot.config_path_text.into());
```

这需要在 `app.rs` 的 `SettingsWindowSnapshot` struct 和 `refresh_settings_window()` 中补充对应字段（见 Task 6）。

- [ ] **Step 8: 更新 refresh_timer，移除 history 相关调用**

将 `refresh_timer` 闭包中的 `apply_history_snapshot(&history, app::refresh_history_window());` 替换为在 settings 可见时调用 `refresh_settings_from_app`（避免每 450ms 无条件刷新历史）：

```rust
if settings.window().is_visible() {
    refresh_settings_from_app(&settings);
} else {
    if let Ok(snapshot) = app::refresh_settings_window() {
        apply_settings_snapshot(&settings, Ok(snapshot));
    }
}
```

- [ ] **Step 9: 编译确认**

```bash
cargo check -p shanji-app 2>&1 | head -60
```

Expected: 无 error

- [ ] **Step 10: Commit**

```bash
git add crates/shanji-slint/src/main.rs
git commit -m "feat(main): 移除 HistoryWindow，连接统一 SettingsWindow 新接口"
```

---

### Task 6: 更新 app.rs — 补全 SettingsWindowSnapshot 字段

**Files:**
- Modify: `crates/shanji-slint/src/app.rs`

- [ ] **Step 1: 读取 config.rs 确认 LLM 相关字段路径**

```bash
grep -n "system_prompt\|base_url\|LlmProvider\|RewriteConfig\|refine_enabled\|overlay" \
  crates/shanji-core/src/config.rs | head -40
```

记录实际字段路径，用于下一步。

- [ ] **Step 2: 在 `SettingsWindowSnapshot` 中添加新字段**

```rust
pub struct SettingsWindowSnapshot {
    // 原有字段保留...
    pub status_text: String,
    pub theme_text: String,
    pub model_text: String,
    pub network_text: String,
    pub audio_device_text: String,
    pub recording_mode_text: String,
    pub rewrite_text: String,
    pub punct_style_text: String,
    pub append_content_text: String,
    pub hotword_summary_text: String,
    pub hotkey_summary_text: String,
    pub overlay_visibility_text: String,
    pub config_path_text: String,
    // 新增字段
    pub llm_enabled: bool,
    pub llm_base_url: String,
    pub llm_model_name: String,
    pub llm_system_prompt: String,
    pub refine_asr_enabled: bool,
    pub overlay_enabled: bool,
}
```

- [ ] **Step 3: 在 `refresh_settings_window()` 中填充新字段**

在 `Ok(SettingsWindowSnapshot { ... })` 中补充：

```rust
llm_enabled: cfg.rewrite.enabled,
llm_base_url: cfg.rewrite.providers.first()
    .map(|p| p.base_url.clone())
    .unwrap_or_default(),
llm_model_name: cfg.rewrite.providers.first()
    .map(|p| p.model.clone())
    .unwrap_or_default(),
// system_prompt 存在于 cfg.rewrite.prompts[active_prompt_id].system_prompt
llm_system_prompt: {
    let active_id = &cfg.rewrite.active_prompt_id;
    cfg.rewrite.prompts.iter()
        .find(|p| &p.id == active_id)
        .map(|p| p.system_prompt.clone())
        .unwrap_or_default()
},
refine_asr_enabled: cfg.asr.refine_enabled,
// overlay_enabled 来自运行时状态而非持久化配置
overlay_enabled: runtime.overlay_visible,
```

> `runtime` 变量在 `refresh_settings_window()` 中已有：`let runtime = state::get_runtime_snapshot();`，直接引用即可。

- [ ] **Step 4: 编译并运行确认主界面正常**

```bash
cargo check -p shanji-app && cargo run -p shanji-app
```

Expected: 应用启动，设置窗口可打开，显示新 UI

- [ ] **Step 5: Commit**

```bash
git add crates/shanji-slint/src/app.rs
git commit -m "feat(app): SettingsWindowSnapshot 补全 LLM/refine/overlay 字段"
```

---

## Chunk 4: 清理 — 删除 HistoryWindow，更新 app-window.slint

### Task 7: 删除 history-window.slint，更新 app-window.slint 中的历史按钮

**Files:**
- Delete: `crates/shanji-slint/ui/history-window.slint`
- Modify: `crates/shanji-slint/ui/app-window.slint`

- [ ] **Step 1: 在 app-window.slint 中确认历史按钮的 callback 名称**

```bash
grep -n "history\|open_history\|open-history" crates/shanji-slint/ui/app-window.slint | head -20
```

- [ ] **Step 2: 确认 app-window.slint 中已有 `on_open_history_requested` callback，无需修改**

Slint 的 `app.on_open_history_requested` 已在 Task 5 Step 2 中连接到新逻辑。`app-window.slint` 中的 callback 声明本身不需要改变（只是 callback 名称和类型）。若文件中有 `HistoryWindow` 的 import 引用，删除之。

- [ ] **Step 3: 删除 history-window.slint**

```bash
rm crates/shanji-slint/ui/history-window.slint
```

- [ ] **Step 4: 确认 Slint 主模块入口不再引用 history-window.slint**

检查所有 `.slint` 文件是否有 `import ... from "history-window.slint"` 引用：

```bash
grep -r "history-window" crates/shanji-slint/ui/
```

若有，删除对应 import 行。

- [ ] **Step 5: 编译确认**

```bash
cargo check -p shanji-app
```

Expected: 无 error

- [ ] **Step 6: 运行冒烟测试**

```bash
cargo run -p shanji-app
```

手动验证：
1. 点击「设置」按钮 → 打开设置窗口，停在「语音识别」页
2. 点击「历史记录」按钮 → 打开设置窗口，切换到「历史记录」页
3. 侧边栏各项目可点击切换
4. LLM 润色页：开关 off 时输入框灰色禁用
5. 历史记录页：可以看到记录卡片（若有数据）

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat: 删除 HistoryWindow，完成统一设置窗口重构"
```

---

### Task 8: 最终收尾 — 移除旧 HistoryWindowSnapshot 和 refresh_history_window

**Files:**
- Modify: `crates/shanji-slint/src/app.rs`

- [ ] **Step 1: 删除 `HistoryWindowSnapshot` struct 和相关函数**

删除以下函数：
- `HistoryWindowSnapshot` struct 定义
- `refresh_history_window()`
- `play_latest_history_audio()`
- `paste_latest_history()`
- `copy_latest_history()`
- `delete_latest_history()`
- `latest_history_record()` 辅助函数（如仅被以上函数使用）

- [ ] **Step 2: 删除 UiSnapshot 中仅用于旧历史窗口的字段**

检查 `UiSnapshot` 中 `history_stats_text` 和 `history_preview_text` 字段是否仍被 `app-window.slint` 的主窗口使用（可能作为主窗口上的统计展示）。若仍需要，保留；若只被旧 `HistoryWindow` 使用，删除。

- [ ] **Step 3: 编译并确认无 error**

```bash
cargo check -p shanji-app
```

- [ ] **Step 4: 运行测试套件**

```bash
cargo test -p shanji-core
cargo test -p shanji-app
```

Expected: 所有测试通过

- [ ] **Step 5: 最终 Commit**

```bash
git add crates/shanji-slint/src/app.rs
git commit -m "chore: 删除旧 HistoryWindowSnapshot 及相关废弃函数"
```

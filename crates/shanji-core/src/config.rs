use crate::error::{AppError, Result};
use crate::paths::AppPaths;
use serde::{Deserialize, Serialize};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock, RwLock};

static CONFIG_CACHE: OnceLock<RwLock<Option<AppConfig>>> = OnceLock::new();
static CONFIG_EVENT_LISTENERS: OnceLock<Mutex<Vec<Sender<()>>>> = OnceLock::new();

pub const LEGACY_DEFAULT_REWRITE_PROMPT_NAME: &str = "语音输入润色";
pub const DEFAULT_REWRITE_PROMPT_NAME: &str = "轻度语音润色";
pub const LEGACY_DEFAULT_REWRITE_SYSTEM_PROMPT: &str =
    "请将以下语音输入文本整理为适合直接输入或粘贴的最终文本：修正明显识别错误，去除语气词、口吃和重复表达，补全自然标点，保留原意，不要无端扩写。只输出整理后的文本。";
pub const DEFAULT_REWRITE_SYSTEM_PROMPT: &str = "请对以下语音输入做轻度整理：只修正非常确定的识别错误、口头语、重复和标点；尽量保留原有句式、顺序、信息粒度和措辞。不要总结，不要改写成说明文，不要补充原文未明确说出的术语全称、示例、背景知识、模型名、产品名或数字。对拿不准的内容宁可保留原样。只输出整理后的文本。";
pub const STRUCTURED_REWRITE_PROMPT_NAME: &str = "结构化";
pub const STRUCTURED_REWRITE_SYSTEM_PROMPT: &str = "请将以下语音输入整理为结构清晰的文本：归纳核心要点，按逻辑顺序组织内容，适当使用编号（1、2、3）或分点，使表达条理清晰；修正识别错误和标点，去除语气词和重复内容。只输出整理后的文本。";
pub const LIGHT_REWRITE_PROMPT_NAME: &str = "轻度整理";
pub const FORMAL_REWRITE_SYSTEM_PROMPT: &str = "请将以下语音输入整理为正式书面文本：修正识别错误，去除口头语和语气词，整理为完整句子，使用标准书面用语，补全标点。适合邮件、报告等正式场合。只输出整理后的文本。";
// kept for v10 migration (not in defaults)
pub const SPOKEN_REWRITE_SYSTEM_PROMPT: &str = "请对以下语音输入只做最小整理：仅修正明显识别错误和错别字，保留口语表达、语气词和原始句式，不改变说话风格。只输出整理后的文本。";
pub const TECHNICAL_REWRITE_SYSTEM_PROMPT: &str = "请将以下语音输入整理为简洁准确的技术表述：修正识别错误，去除冗余措辞，保留专业术语和所有技术细节，不添加解释或背景信息。只输出整理后的文本。";

fn config_cache() -> &'static RwLock<Option<AppConfig>> {
    CONFIG_CACHE.get_or_init(|| RwLock::new(None))
}

fn config_event_listeners() -> &'static Mutex<Vec<Sender<()>>> {
    CONFIG_EVENT_LISTENERS.get_or_init(|| Mutex::new(Vec::new()))
}

fn notify_config_changed() {
    let mut listeners = config_event_listeners().lock().unwrap();
    listeners.retain(|tx| tx.send(()).is_ok());
}

pub fn subscribe_config_events() -> Receiver<()> {
    let (tx, rx) = mpsc::channel();
    config_event_listeners().lock().unwrap().push(tx);
    rx
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum AppState {
    Idle,
    Recording,
    Transcribing,
    Rewriting,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GeneralConfig {
    pub launch_at_startup: bool,
    pub minimize_to_tray: bool,
    pub language: String,
    pub theme: String,
    pub auto_update: bool,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            launch_at_startup: true,
            minimize_to_tray: true,
            language: "zh".to_string(),
            theme: "system".to_string(),
            auto_update: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AudioConfig {
    pub device_name: Option<String>,
    pub gain: f32,
    pub recording_mode: String,
    pub vad_threshold: f32,
    pub vad_end_threshold: f32,
    pub silence_timeout_ms: u32,
    pub min_speech_frames: u32,
    pub sound_feedback: bool,
    pub noise_reduction: bool,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            device_name: None,
            gain: 1.0,
            #[cfg(target_os = "macos")]
            recording_mode: "push-to-talk".to_string(),
            #[cfg(not(target_os = "macos"))]
            recording_mode: "toggle".to_string(),
            vad_threshold: 0.45,
            vad_end_threshold: 0.3,
            silence_timeout_ms: 1500,
            min_speech_frames: 2,
            sound_feedback: true,
            noise_reduction: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AsrConfig {
    pub live_model_id: String,
    pub refine_enabled: bool,
    pub refine_model_id: String,
    pub insert_punct: bool,
    pub punct_style: String,
    pub comma_pause_ms: u32,
    pub sentence_pause_ms: u32,
}

impl Default for AsrConfig {
    fn default() -> Self {
        Self {
            live_model_id: "paraformer-zh-streaming".to_string(),
            refine_enabled: false,
            refine_model_id: "paraformer-zh".to_string(),
            insert_punct: true,
            punct_style: "zh".to_string(),
            comma_pause_ms: 800,
            sentence_pause_ms: 2_800,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmProvider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub api_key_encrypted: String,
    /// 0=未测试 1=成功 2=失败
    #[serde(default)]
    pub test_status: i32,
    #[serde(default)]
    pub test_status_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptPreset {
    pub id: String,
    pub name: String,
    pub system_prompt: String,
    pub is_builtin: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RewriteConfig {
    pub enabled: bool,
    pub mode: String,
    pub active_provider_id: String,
    pub providers: Vec<LlmProvider>,
    pub active_prompt_id: String,
    pub prompts: Vec<PromptPreset>,
}

impl Default for RewriteConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: "auto".to_string(),
            active_provider_id: "".to_string(),
            providers: vec![],
            active_prompt_id: "default".to_string(),
            prompts: vec![
                PromptPreset {
                    id: "default".to_string(),
                    name: STRUCTURED_REWRITE_PROMPT_NAME.to_string(),
                    system_prompt: STRUCTURED_REWRITE_SYSTEM_PROMPT.to_string(),
                    is_builtin: true,
                },
                PromptPreset {
                    id: "light".to_string(),
                    name: LIGHT_REWRITE_PROMPT_NAME.to_string(),
                    system_prompt: DEFAULT_REWRITE_SYSTEM_PROMPT.to_string(),
                    is_builtin: true,
                },
                PromptPreset {
                    id: "formal".to_string(),
                    name: "书面化".to_string(),
                    system_prompt: FORMAL_REWRITE_SYSTEM_PROMPT.to_string(),
                    is_builtin: true,
                },
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OutputConfig {
    pub restore_clipboard: bool,
    pub append_content: String,
    pub punct_style: String,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            restore_clipboard: true,
            append_content: "space".to_string(),
            punct_style: "zh".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HotkeyConfig {
    pub toggle_recording: String,
    pub push_to_talk: String,
    #[serde(default = "default_push_to_talk_hold_delay_ms")]
    pub push_to_talk_hold_delay_ms: u32,
    pub toggle_rewrite: String,
    pub open_history: String,
    pub open_main: String,
}

fn default_push_to_talk_hold_delay_ms() -> u32 {
    #[cfg(target_os = "macos")]
    {
        return 500;
    }

    #[cfg(not(target_os = "macos"))]
    {
        0
    }
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self {
                toggle_recording: String::new(),
                push_to_talk: "RightCommand".to_string(),
                push_to_talk_hold_delay_ms: 500,
                toggle_rewrite: String::new(),
                open_history: String::new(),
                open_main: String::new(),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            Self {
                toggle_recording: "Alt+Space".to_string(),
                push_to_talk: "Alt+R".to_string(),
                push_to_talk_hold_delay_ms: 0,
                toggle_rewrite: "Ctrl+Shift+R".to_string(),
                open_history: "Ctrl+Shift+H".to_string(),
                open_main: "Ctrl+Shift+S".to_string(),
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HistoryConfig {
    pub max_records: u32,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self { max_records: 100 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OverlayConfig {
    pub position: String,
    pub custom_x: Option<i32>,
    pub custom_y: Option<i32>,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            position: "top-right".to_string(),
            custom_x: None,
            custom_y: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NetworkConfig {
    pub github_proxy: String,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            github_proxy: "https://ghfast.top/".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    pub general: GeneralConfig,
    pub audio: AudioConfig,
    pub asr: AsrConfig,
    pub rewrite: RewriteConfig,
    pub output: OutputConfig,
    pub hotkeys: HotkeyConfig,
    pub history: HistoryConfig,
    pub overlay: OverlayConfig,
    pub network: NetworkConfig,
}

fn default_version() -> u32 {
    0
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: 11,
            general: GeneralConfig::default(),
            audio: AudioConfig::default(),
            asr: AsrConfig::default(),
            rewrite: RewriteConfig::default(),
            output: OutputConfig::default(),
            hotkeys: HotkeyConfig::default(),
            history: HistoryConfig::default(),
            overlay: OverlayConfig::default(),
            network: NetworkConfig::default(),
        }
    }
}

impl AppConfig {
    pub fn migrate(&mut self) -> bool {
        let mut modified = false;

        if self.version < 1 {
            if ["Alt+Space", "Fn", "F13"].contains(&self.hotkeys.toggle_recording.as_str()) {
                self.hotkeys.toggle_recording = "Option".to_string();
            }
            if ["Alt+R", "Fn", "F13"].contains(&self.hotkeys.push_to_talk.as_str()) {
                self.hotkeys.push_to_talk = "Option".to_string();
            }
            self.version = 1;
            modified = true;
        }

        if self.version < 2 {
            let default_hotkeys = HotkeyConfig::default();
            let mut hotkeys_updated = false;

            if needs_native_hotkey_migration(&self.hotkeys.toggle_recording) {
                self.hotkeys.toggle_recording = default_hotkeys.toggle_recording;
                hotkeys_updated = true;
            }
            if needs_native_hotkey_migration(&self.hotkeys.push_to_talk) {
                self.hotkeys.push_to_talk = default_hotkeys.push_to_talk;
                hotkeys_updated = true;
            }

            self.version = 2;
            modified = modified || hotkeys_updated || self.version == 2;
        }

        if self.version < 3 {
            // vad_threshold 旧含义为振幅阈值（约 0.05），新含义为 Silero 概率（0.0-1.0）
            if self.audio.vad_threshold < 0.1 {
                self.audio.vad_threshold = 0.5;
            }
            self.version = 3;
            modified = true;
        }

        if self.version < 4 {
            if self.network.github_proxy.trim().is_empty() {
                self.network.github_proxy = NetworkConfig::default().github_proxy;
            }
            self.version = 4;
            modified = true;
        }

        if self.version < 5 {
            if !(0.0..=1.0).contains(&self.audio.vad_end_threshold)
                || self.audio.vad_end_threshold <= 0.0
            {
                self.audio.vad_end_threshold =
                    (self.audio.vad_threshold * 0.7).clamp(0.2, self.audio.vad_threshold);
            }
            if self.audio.min_speech_frames == 0 {
                self.audio.min_speech_frames = 3;
            }
            self.version = 5;
            modified = true;
        }

        if self.version < 6 {
            let uses_legacy_vad_defaults = approx_eq(self.audio.vad_threshold, 0.5)
                && approx_eq(self.audio.vad_end_threshold, 0.35)
                && self.audio.min_speech_frames == 3;
            if uses_legacy_vad_defaults {
                self.audio.vad_threshold = 0.45;
                self.audio.vad_end_threshold = 0.3;
                self.audio.min_speech_frames = 2;
            }
            self.version = 6;
            modified = true;
        }

        if self.version < 7 {
            let default_asr = AsrConfig::default();
            if self.asr.comma_pause_ms == 0 {
                self.asr.comma_pause_ms = default_asr.comma_pause_ms;
            }
            if self.asr.sentence_pause_ms == 0 {
                self.asr.sentence_pause_ms = default_asr.sentence_pause_ms;
            }
            if self.asr.sentence_pause_ms <= self.asr.comma_pause_ms {
                self.asr.sentence_pause_ms = self
                    .asr
                    .comma_pause_ms
                    .saturating_add(1_000)
                    .max(default_asr.sentence_pause_ms);
            }

            let uses_legacy_one_frame_vad = approx_eq(self.audio.vad_threshold, 0.5)
                && approx_eq(self.audio.vad_end_threshold, 0.35)
                && self.audio.min_speech_frames == 1;
            if uses_legacy_one_frame_vad {
                self.audio.vad_threshold = 0.45;
                self.audio.vad_end_threshold = 0.3;
                self.audio.min_speech_frames = 2;
            }

            self.version = 7;
            modified = true;
        }

        if self.version < 8 {
            if self.rewrite.active_prompt_id.trim().is_empty() {
                self.rewrite.active_prompt_id = "default".to_string();
            }

            if self.rewrite.prompts.is_empty() {
                self.rewrite.prompts.push(PromptPreset {
                    id: "default".to_string(),
                    name: DEFAULT_REWRITE_PROMPT_NAME.to_string(),
                    system_prompt: DEFAULT_REWRITE_SYSTEM_PROMPT.to_string(),
                    is_builtin: true,
                });
            }

            if let Some(preset) = self
                .rewrite
                .prompts
                .iter_mut()
                .find(|preset| preset.id == "default" && preset.is_builtin)
            {
                if preset.system_prompt.trim().is_empty()
                    || preset.system_prompt == LEGACY_DEFAULT_REWRITE_SYSTEM_PROMPT
                {
                    preset.name = DEFAULT_REWRITE_PROMPT_NAME.to_string();
                    preset.system_prompt = DEFAULT_REWRITE_SYSTEM_PROMPT.to_string();
                } else if preset.name == LEGACY_DEFAULT_REWRITE_PROMPT_NAME {
                    preset.name = DEFAULT_REWRITE_PROMPT_NAME.to_string();
                }
            }

            self.version = 8;
            modified = true;
        }

        if self.version < 9 {
            let used_legacy_pause_defaults =
                self.asr.comma_pause_ms == 1_200 && self.asr.sentence_pause_ms == 2_800;
            if used_legacy_pause_defaults {
                self.asr.comma_pause_ms = AsrConfig::default().comma_pause_ms;
            }

            if self.asr.sentence_pause_ms <= self.asr.comma_pause_ms {
                self.asr.sentence_pause_ms = self
                    .asr
                    .comma_pause_ms
                    .saturating_add(1_000)
                    .max(AsrConfig::default().sentence_pause_ms);
            }

            self.version = 9;
            modified = true;
        }

        if self.version < 10 {
            let new_builtins = [
                ("formal", "书面化润色", FORMAL_REWRITE_SYSTEM_PROMPT),
                ("spoken", "口语保留", SPOKEN_REWRITE_SYSTEM_PROMPT),
                ("technical", "技术简洁", TECHNICAL_REWRITE_SYSTEM_PROMPT),
            ];
            for (id, name, prompt) in &new_builtins {
                if !self.rewrite.prompts.iter().any(|p| p.id == *id) {
                    self.rewrite.prompts.push(PromptPreset {
                        id: id.to_string(),
                        name: name.to_string(),
                        system_prompt: prompt.to_string(),
                        is_builtin: true,
                    });
                }
            }
            self.version = 10;
            modified = true;
        }

        if self.version < 11 {
            // Update "default" builtin to 结构化 (only if it still has old light-editing prompt)
            if let Some(p) = self.rewrite.prompts.iter_mut().find(|p| {
                p.id == "default"
                    && p.is_builtin
                    && (p.system_prompt == DEFAULT_REWRITE_SYSTEM_PROMPT
                        || p.system_prompt == LEGACY_DEFAULT_REWRITE_SYSTEM_PROMPT)
            }) {
                p.name = STRUCTURED_REWRITE_PROMPT_NAME.to_string();
                p.system_prompt = STRUCTURED_REWRITE_SYSTEM_PROMPT.to_string();
            }
            // Add "light" preset if not present
            if !self.rewrite.prompts.iter().any(|p| p.id == "light") {
                self.rewrite.prompts.push(PromptPreset {
                    id: "light".to_string(),
                    name: LIGHT_REWRITE_PROMPT_NAME.to_string(),
                    system_prompt: DEFAULT_REWRITE_SYSTEM_PROMPT.to_string(),
                    is_builtin: true,
                });
            }
            // Remove obsolete builtin presets
            self.rewrite
                .prompts
                .retain(|p| !p.is_builtin || (p.id != "spoken" && p.id != "technical"));
            // Reset active prompt if it pointed to removed presets
            if self.rewrite.active_prompt_id == "spoken"
                || self.rewrite.active_prompt_id == "technical"
            {
                self.rewrite.active_prompt_id = "default".to_string();
            }
            self.version = 11;
            modified = true;
        }

        modified
    }
}

fn approx_eq(lhs: f32, rhs: f32) -> bool {
    (lhs - rhs).abs() <= 0.0001
}

fn needs_native_hotkey_migration(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "command"
            | "cmd"
            | "meta"
            | "option"
            | "alt"
            | "opt"
            | "alt+space"
            | "alt+r"
            | "fn"
            | "f13"
    )
}

pub fn init_config(paths: &AppPaths) -> Result<()> {
    paths.ensure_base_dirs()?;
    crate::hotwords::ensure_default_libraries_with_paths(paths)?;

    let config_path = paths.config_file();
    if !config_path.exists() {
        let default_config = AppConfig::default();
        save_config(paths, &default_config)?;
    }

    let mut config = get_config(paths)?;
    if config.migrate() {
        save_config(paths, &config)?;
    }

    Ok(())
}

pub fn get_config(paths: &AppPaths) -> Result<AppConfig> {
    // 先查内存缓存，命中则直接返回，避免每次读磁盘
    if let Some(cached) = config_cache().read().unwrap().as_ref() {
        return Ok(cached.clone());
    }

    let config_path = paths.config_file();
    let config = if config_path.exists() {
        let content =
            std::fs::read_to_string(config_path).map_err(|e| AppError::Io(e.to_string()))?;
        serde_json::from_str(&content).map_err(|e| AppError::Config(e.to_string()))?
    } else {
        AppConfig::default()
    };

    *config_cache().write().unwrap() = Some(config.clone());
    Ok(config)
}

pub fn save_config(paths: &AppPaths, config: &AppConfig) -> Result<()> {
    let json = serde_json::to_string_pretty(config).map_err(|e| AppError::Config(e.to_string()))?;
    std::fs::write(paths.config_file(), json).map_err(|e| AppError::Io(e.to_string()))?;
    // 写入后同步更新内存缓存，下次 get_config 直接命中
    *config_cache().write().unwrap() = Some(config.clone());
    notify_config_changed();
    Ok(())
}

pub fn reset_config(paths: &AppPaths) -> Result<AppConfig> {
    let default_config = AppConfig::default();
    save_config(paths, &default_config)?;
    Ok(default_config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_updates_legacy_vad_defaults() {
        let mut config = AppConfig::default();
        config.version = 5;
        config.audio.vad_threshold = 0.5;
        config.audio.vad_end_threshold = 0.35;
        config.audio.min_speech_frames = 3;

        assert!(config.migrate());
        assert_eq!(config.version, 11);
        assert!(approx_eq(config.audio.vad_threshold, 0.45));
        assert!(approx_eq(config.audio.vad_end_threshold, 0.3));
        assert_eq!(config.audio.min_speech_frames, 2);
        assert_eq!(config.asr.comma_pause_ms, 800);
        assert_eq!(config.asr.sentence_pause_ms, 2_800);
    }

    #[test]
    fn migrate_preserves_custom_vad_tuning() {
        let mut config = AppConfig::default();
        config.version = 5;
        config.audio.vad_threshold = 0.55;
        config.audio.vad_end_threshold = 0.25;
        config.audio.min_speech_frames = 4;
        config.asr.comma_pause_ms = 900;
        config.asr.sentence_pause_ms = 2_400;

        assert!(config.migrate());
        assert_eq!(config.version, 11);
        assert!(approx_eq(config.audio.vad_threshold, 0.55));
        assert!(approx_eq(config.audio.vad_end_threshold, 0.25));
        assert_eq!(config.audio.min_speech_frames, 4);
        assert_eq!(config.asr.comma_pause_ms, 900);
        assert_eq!(config.asr.sentence_pause_ms, 2_400);
    }

    #[test]
    fn migrate_updates_legacy_one_frame_vad_profile() {
        let mut config = AppConfig::default();
        config.version = 6;
        config.audio.vad_threshold = 0.5;
        config.audio.vad_end_threshold = 0.35;
        config.audio.min_speech_frames = 1;

        assert!(config.migrate());
        assert_eq!(config.version, 11);
        assert!(approx_eq(config.audio.vad_threshold, 0.45));
        assert!(approx_eq(config.audio.vad_end_threshold, 0.3));
        assert_eq!(config.audio.min_speech_frames, 2);
    }

    #[test]
    fn migrate_normalizes_invalid_pause_threshold_order() {
        let mut config = AppConfig::default();
        config.version = 6;
        config.asr.comma_pause_ms = 1_800;
        config.asr.sentence_pause_ms = 1_000;

        assert!(config.migrate());
        assert_eq!(config.version, 11);
        assert!(config.asr.sentence_pause_ms > config.asr.comma_pause_ms);
    }

    #[test]
    fn migrate_updates_legacy_default_rewrite_prompt() {
        let mut config = AppConfig::default();
        config.version = 7;
        config.rewrite.prompts = vec![PromptPreset {
            id: "default".to_string(),
            name: LEGACY_DEFAULT_REWRITE_PROMPT_NAME.to_string(),
            system_prompt: LEGACY_DEFAULT_REWRITE_SYSTEM_PROMPT.to_string(),
            is_builtin: true,
        }];

        assert!(config.migrate());
        assert_eq!(config.version, 11);
        assert_eq!(config.rewrite.prompts[0].name, STRUCTURED_REWRITE_PROMPT_NAME);
        assert_eq!(
            config.rewrite.prompts[0].system_prompt,
            STRUCTURED_REWRITE_SYSTEM_PROMPT
        );
    }

    #[test]
    fn migrate_preserves_custom_rewrite_prompt() {
        let mut config = AppConfig::default();
        config.version = 7;
        config.rewrite.prompts = vec![PromptPreset {
            id: "default".to_string(),
            name: "我的自定义润色".to_string(),
            system_prompt: "请把语气整理得更轻松，但不要改我的内容".to_string(),
            is_builtin: true,
        }];

        assert!(config.migrate());
        assert_eq!(config.version, 11);
        assert_eq!(config.rewrite.prompts[0].name, "我的自定义润色");
        assert_eq!(
            config.rewrite.prompts[0].system_prompt,
            "请把语气整理得更轻松，但不要改我的内容"
        );
    }

    #[test]
    fn migrate_updates_legacy_pause_defaults_to_lower_comma_threshold() {
        let mut config = AppConfig::default();
        config.version = 8;
        config.asr.comma_pause_ms = 1_200;
        config.asr.sentence_pause_ms = 2_800;

        assert!(config.migrate());
        assert_eq!(config.version, 11);
        assert_eq!(config.asr.comma_pause_ms, 800);
        assert_eq!(config.asr.sentence_pause_ms, 2_800);
    }

    #[test]
    fn migrate_preserves_custom_pause_thresholds() {
        let mut config = AppConfig::default();
        config.version = 8;
        config.asr.comma_pause_ms = 900;
        config.asr.sentence_pause_ms = 2_400;

        assert!(config.migrate());
        assert_eq!(config.version, 11);
        assert_eq!(config.asr.comma_pause_ms, 900);
        assert_eq!(config.asr.sentence_pause_ms, 2_400);
    }
}

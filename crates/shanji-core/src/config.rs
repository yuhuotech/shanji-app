use crate::error::{AppError, Result};
use crate::paths::AppPaths;
use serde::{Deserialize, Serialize};

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
            recording_mode: "toggle".to_string(),
            vad_threshold: 0.5,
            silence_timeout_ms: 1500,
            min_speech_frames: 1,
            sound_feedback: true,
            noise_reduction: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AsrConfig {
    pub model_id: String,
    pub insert_punct: bool,
    pub punct_style: String,
}

impl Default for AsrConfig {
    fn default() -> Self {
        Self {
            model_id: "paraformer-zh".to_string(),
            insert_punct: true,
            punct_style: "zh".to_string(),
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
            prompts: vec![PromptPreset {
                id: "default".to_string(),
                name: "通用书面化".to_string(),
                system_prompt:
                    "请将以下口语化的语音转写文本改写为书面语，去除语气词和重复内容，保持原意："
                        .to_string(),
                is_builtin: true,
            }],
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
    pub toggle_rewrite: String,
    pub open_history: String,
    pub open_main: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self {
                toggle_recording: "Cmd+Shift+Space".to_string(),
                push_to_talk: "Option+Space".to_string(),
                toggle_rewrite: "Cmd+Shift+R".to_string(),
                open_history: "Cmd+Shift+H".to_string(),
                open_main: "Cmd+Shift+S".to_string(),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            Self {
                toggle_recording: "Alt+Space".to_string(),
                push_to_talk: "Alt+R".to_string(),
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
}

fn default_version() -> u32 {
    0
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: 1,
            general: GeneralConfig::default(),
            audio: AudioConfig::default(),
            asr: AsrConfig::default(),
            rewrite: RewriteConfig::default(),
            output: OutputConfig::default(),
            hotkeys: HotkeyConfig::default(),
            history: HistoryConfig::default(),
            overlay: OverlayConfig::default(),
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

        modified
    }
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
    let config_path = paths.config_file();

    if !config_path.exists() {
        return Ok(AppConfig::default());
    }

    let content = std::fs::read_to_string(config_path).map_err(|e| AppError::Io(e.to_string()))?;
    let config = serde_json::from_str(&content).map_err(|e| AppError::Config(e.to_string()))?;
    Ok(config)
}

pub fn save_config(paths: &AppPaths, config: &AppConfig) -> Result<()> {
    let json = serde_json::to_string_pretty(config).map_err(|e| AppError::Config(e.to_string()))?;
    std::fs::write(paths.config_file(), json).map_err(|e| AppError::Io(e.to_string()))?;
    Ok(())
}

pub fn reset_config(paths: &AppPaths) -> Result<AppConfig> {
    let default_config = AppConfig::default();
    save_config(paths, &default_config)?;
    Ok(default_config)
}

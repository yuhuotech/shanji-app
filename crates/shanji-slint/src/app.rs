use shanji_core::audio;
use shanji_core::config::{self, AppConfig, AppState};
use shanji_core::history::HistoryDb;
use shanji_core::hotwords;
use shanji_core::model;
use shanji_core::paths::AppPaths;
use shanji_core::state::{self, RuntimeSnapshot};
use shanji_platform::{hotkeys, tray};

#[allow(dead_code)]
pub struct UiSnapshot {
    pub app_name: String,
    pub app_subtitle_text: String,
    pub status_text: String,
    pub hotkey_hint_title: String,
    pub hotkey_hint_body: String,
    pub hotkey_display_text: String,
    pub model_title_text: String,
    pub model_desc_text: String,
    pub model_version_text: String,
    pub model_size_text: String,
    pub model_language_text: String,
    pub model_install_text: String,
    pub model_ready: bool,
    pub model_downloading: bool,
    pub model_download_progress: f32,
    pub model_download_status_text: String,
    pub model_download_error_text: String,
    pub github_proxy_index: i32,
    pub mic_permission_title: String,
    pub mic_permission_body: String,
    pub mic_ready: bool,
    pub paste_permission_title: String,
    pub paste_permission_body: String,
    pub paste_ready: bool,
    pub default_device_text: String,
    pub transcribe_card_title: String,
    pub transcribe_button_text: String,
    pub transcribe_body_text: String,
    pub audio_level_text: String,
    pub audio_level_value: f32,
    pub monitor_button_text: String,
    pub monitor_tip_text: String,
    pub state_text: String,
    pub model_text: String,
    pub model_summary_text: String,
    pub model_inventory_text: String,
    pub theme_text: String,
    pub audio_device_text: String,
    pub recording_mode_text: String,
    pub rewrite_text: String,
    pub punct_style_text: String,
    pub append_content_text: String,
    pub hotword_summary_text: String,
    pub hotword_inventory_text: String,
    pub hotkey_summary_text: String,
    pub hotkey_inventory_text: String,
    pub tray_summary_text: String,
    pub tray_inventory_text: String,
    pub live_asr_text: String,
    pub overlay_visible: bool,
    pub overlay_visibility_text: String,
    pub config_path_text: String,
    pub history_stats_text: String,
    pub history_preview_text: String,
    pub settings_summary_text: String,
    pub live_session_text: String,
    pub output_preview_text: String,
    pub overlay_state_text: String,
    pub overlay_body_text: String,
}

pub struct SettingsWindowSnapshot {
    pub theme_text: String,
    pub model_text: String,
    pub audio_device_text: String,
    pub recording_mode_text: String,
    pub punct_style_text: String,
    pub append_content_text: String,
    pub hotword_summary_text: String,
    pub hotkey_summary_text: String,
    pub config_path_text: String,
    pub llm_enabled: bool,
    pub llm_base_url: String,
    pub llm_model_name: String,
    pub llm_system_prompt: String,
    pub refine_asr_enabled: bool,
    pub overlay_enabled: bool,
}

const GITHUB_PROXY_VALUES: [&str; 3] = ["https://ghfast.top/", "https://gh-proxy.com/", ""];

pub fn bootstrap_snapshot() -> UiSnapshot {
    shanji_core::state::init_state();

    match refresh_snapshot() {
        Ok(snapshot) => snapshot,
        Err(err) => UiSnapshot {
            app_name: "闪记".to_string(),
            app_subtitle_text: "AI 语音输入工具".to_string(),
            status_text: "Core bootstrap failed".to_string(),
            hotkey_hint_title: "按下快捷键开始语音输入".to_string(),
            hotkey_hint_body: "把光标放到目标输入框，按住说话，松开后自动转写并粘贴".to_string(),
            hotkey_display_text: "Unavailable".to_string(),
            model_title_text: "模型不可用".to_string(),
            model_desc_text: "当前无法读取模型信息".to_string(),
            model_version_text: "N/A".to_string(),
            model_size_text: "N/A".to_string(),
            model_language_text: "N/A".to_string(),
            model_install_text: "模型未就绪".to_string(),
            model_ready: false,
            model_downloading: false,
            model_download_progress: 0.0,
            model_download_status_text: String::new(),
            model_download_error_text: String::new(),
            github_proxy_index: 0,
            mic_permission_title: "麦克风状态不可用".to_string(),
            mic_permission_body: "当前无法读取音频输入设备".to_string(),
            mic_ready: false,
            paste_permission_title: "自动粘贴状态不可用".to_string(),
            paste_permission_body: "当前无法读取输出链路状态".to_string(),
            paste_ready: false,
            default_device_text: "Unknown".to_string(),
            transcribe_card_title: "语音转写测试".to_string(),
            transcribe_button_text: "点击测试录音".to_string(),
            transcribe_body_text: "测试转写功能是否正常".to_string(),
            audio_level_text: "0%".to_string(),
            audio_level_value: 0.0,
            monitor_button_text: "监听麦克风输入".to_string(),
            monitor_tip_text: "提示：日常使用无需打开此窗口，直接按快捷键即可".to_string(),
            state_text: "Unavailable".to_string(),
            model_text: "Unavailable".to_string(),
            model_summary_text: "Models unavailable".to_string(),
            model_inventory_text: "Model inventory unavailable".to_string(),
            theme_text: "Unavailable".to_string(),
            audio_device_text: "Audio devices unavailable".to_string(),
            recording_mode_text: "Unavailable".to_string(),
            rewrite_text: "LLM polish: unavailable".to_string(),
            punct_style_text: "Punct style: unavailable".to_string(),
            append_content_text: "Append content: unavailable".to_string(),
            hotword_summary_text: "Hotwords unavailable".to_string(),
            hotword_inventory_text: "Hotword inventory unavailable".to_string(),
            hotkey_summary_text: "Hotkeys unavailable".to_string(),
            hotkey_inventory_text: "Hotkey inventory unavailable".to_string(),
            tray_summary_text: "Tray unavailable".to_string(),
            tray_inventory_text: "Tray inventory unavailable".to_string(),
            live_asr_text: "Live ASR unavailable".to_string(),
            overlay_visible: false,
            overlay_visibility_text: "Overlay unavailable".to_string(),
            config_path_text: err,
            history_stats_text: "History stats unavailable".to_string(),
            history_preview_text: "History unavailable".to_string(),
            settings_summary_text: "设置摘要不可用".to_string(),
            live_session_text: "Live session unavailable".to_string(),
            output_preview_text: "Output preview unavailable".to_string(),
            overlay_state_text: "Overlay unavailable".to_string(),
            overlay_body_text: "Shared core bootstrap failed".to_string(),
        },
    }
}

pub fn refresh_snapshot() -> Result<UiSnapshot, String> {
    let paths = resolve_app_paths();
    config::init_config(&paths).map_err(|e| e.to_string())?;
    let cfg = config::get_config(&paths).map_err(|e| e.to_string())?;
    state::set_active_model(cfg.asr.live_model_id.clone());
    let runtime = state::get_runtime_snapshot();
    let active_model = active_live_model_info(&paths, &cfg);
    let refine_model = refine_model_info(&paths, &cfg);
    let live_model_downloaded = active_model
        .as_ref()
        .map(|m| m.is_downloaded)
        .unwrap_or(false);
    let refine_model_downloaded = refine_model
        .as_ref()
        .map(|m| m.is_downloaded)
        .unwrap_or(false);
    let live_model_downloading = crate::model_downloader::is_downloading(&cfg.asr.live_model_id);
    let refine_model_downloading =
        crate::model_downloader::is_downloading(&cfg.asr.refine_model_id);
    let audio_devices = audio::list_input_devices().unwrap_or_default();
    let selected_device = selected_audio_device_name(&cfg, &audio_devices);
    let hotkey_display = cfg.hotkeys.toggle_recording.clone();
    let audio_level = runtime.audio_level.clamp(0.0, 1.0);

    Ok(UiSnapshot {
        app_name: "闪记".to_string(),
        app_subtitle_text: "AI 语音输入工具".to_string(),
        status_text: runtime.status_message.clone(),
        hotkey_hint_title: "按下快捷键开始语音输入".to_string(),
        hotkey_hint_body: match cfg.audio.recording_mode.as_str() {
            "push-to-talk" => "把光标放到目标输入框，按住说话，松开后自动转写并粘贴".to_string(),
            _ => "把光标放到目标输入框，按一次开始说话，再按一次结束转写并粘贴".to_string(),
        },
        hotkey_display_text: hotkey_display,
        model_title_text: active_model
            .as_ref()
            .map(|model| model.name.clone())
            .unwrap_or_else(|| cfg.asr.live_model_id.clone()),
        model_desc_text: format!(
            "{}\n整体纠正: {}",
            active_model
                .as_ref()
                .map(|model| model.description.clone())
                .unwrap_or_else(|| "当前模型描述不可用".to_string()),
            refine_runtime_summary(&cfg, refine_model_downloaded, refine_model_downloading)
        ),
        model_version_text: active_model
            .as_ref()
            .map(|model| model.version.clone())
            .unwrap_or_else(|| "N/A".to_string()),
        model_size_text: active_model
            .as_ref()
            .map(|model| human_readable_bytes(model.size_bytes))
            .unwrap_or_else(|| "N/A".to_string()),
        model_language_text: active_model
            .as_ref()
            .map(|model| model.language.to_uppercase())
            .unwrap_or_else(|| "N/A".to_string()),
        model_install_text: format!(
            "实时模型{} / 整体纠正{}",
            if live_model_downloaded {
                "已安装"
            } else {
                "未安装"
            },
            if cfg.asr.refine_enabled {
                if refine_model_downloaded {
                    "已启用"
                } else {
                    "未就绪"
                }
            } else {
                "未启用"
            }
        ),
        model_ready: live_model_downloaded,
        model_downloading: live_model_downloading,
        model_download_progress: crate::model_downloader::get_progress(&cfg.asr.live_model_id),
        model_download_status_text: crate::model_downloader::get_status_text(
            &cfg.asr.live_model_id,
        ),
        model_download_error_text: crate::model_downloader::get_last_error(&cfg.asr.live_model_id)
            .unwrap_or_default(),
        github_proxy_index: github_proxy_index(&cfg),
        mic_permission_title: {
            use shanji_core::audio::{get_mic_permission_status, MicPermissionStatus};
            match get_mic_permission_status() {
                MicPermissionStatus::Authorized => {
                    if audio_devices.is_empty() {
                        "未检测到麦克风输入设备".to_string()
                    } else {
                        "麦克风权限已获取".to_string()
                    }
                }
                MicPermissionStatus::Denied => "麦克风权限已拒绝".to_string(),
                MicPermissionStatus::NotDetermined => "麦克风尚未授权".to_string(),
                MicPermissionStatus::Restricted => "麦克风权限受限".to_string(),
            }
        },
        mic_permission_body: {
            use shanji_core::audio::{get_mic_permission_status, MicPermissionStatus};
            match get_mic_permission_status() {
                MicPermissionStatus::Authorized => {
                    if audio_devices.is_empty() {
                        "请检查系统麦克风权限与音频设备连接状态".to_string()
                    } else {
                        format!("已检测到 {} 个可用输入设备", audio_devices.len())
                    }
                }
                MicPermissionStatus::Denied => "已在系统设置中拒绝，请前往授权".to_string(),
                MicPermissionStatus::NotDetermined => "首次使用需要授权麦克风访问".to_string(),
                MicPermissionStatus::Restricted => "设备管理策略限制了麦克风访问".to_string(),
            }
        },
        mic_ready: {
            use shanji_core::audio::{get_mic_permission_status, MicPermissionStatus};
            get_mic_permission_status() == MicPermissionStatus::Authorized
                && !audio_devices.is_empty()
        },
        paste_permission_title: if shanji_core::output::is_auto_paste_supported() {
            "自动粘贴已启用".to_string()
        } else {
            "自动粘贴不可用".to_string()
        },
        paste_permission_body: shanji_core::output::output_method_recommendation().to_string(),
        paste_ready: shanji_core::output::is_auto_paste_supported(),
        default_device_text: selected_device,
        transcribe_card_title: "语音转写测试".to_string(),
        transcribe_button_text: if crate::audio_transcriber::is_running() {
            "停止测试录音".to_string()
        } else {
            "点击测试录音".to_string()
        },
        transcribe_body_text: if crate::audio_transcriber::is_running() {
            "正在通过真实麦克风和当前模型进行转写".to_string()
        } else {
            "测试转写功能是否正常".to_string()
        },
        audio_level_text: format!("{}%", (audio_level * 100.0).round() as u32),
        audio_level_value: audio_level,
        monitor_button_text: if crate::audio_monitor::is_running() {
            "停止监听麦克风输入".to_string()
        } else {
            "监听麦克风输入".to_string()
        },
        monitor_tip_text: "提示：日常使用无需打开此窗口，直接按快捷键即可".to_string(),
        state_text: format_state(runtime.current_state.clone()),
        model_text: format!("Live model: {}", cfg.asr.live_model_id),
        model_summary_text: load_model_summary(&paths, &cfg),
        model_inventory_text: load_model_inventory(&paths, &cfg),
        theme_text: format!("Theme: {}", cfg.general.theme),
        audio_device_text: load_audio_device_summary(&cfg),
        recording_mode_text: format!("Recording mode: {}", cfg.audio.recording_mode),
        rewrite_text: format!(
            "LLM polish: {}",
            if cfg.rewrite.enabled {
                "enabled"
            } else {
                "disabled"
            }
        ),
        punct_style_text: format!("Punct style: {}", cfg.output.punct_style),
        append_content_text: format!("Append content: {}", cfg.output.append_content),
        hotword_summary_text: load_hotword_summary(&paths),
        hotword_inventory_text: load_hotword_inventory(&paths),
        hotkey_summary_text: load_hotkey_summary(&cfg),
        hotkey_inventory_text: load_hotkey_inventory(&cfg),
        tray_summary_text: load_tray_summary(&cfg, &runtime),
        tray_inventory_text: load_tray_inventory(&cfg, &runtime),
        live_asr_text: load_live_asr_summary(&cfg, &runtime),
        overlay_visible: runtime.overlay_visible
            && matches!(
                runtime.current_state,
                AppState::Recording | AppState::Transcribing | AppState::Rewriting
            ),
        overlay_visibility_text: if runtime.overlay_visible {
            "Overlay: visible".to_string()
        } else {
            "Overlay: hidden".to_string()
        },
        config_path_text: format!("Config: {}", paths.config_file().display()),
        history_stats_text: load_history_stats(&paths),
        history_preview_text: load_history_preview(&paths),
        settings_summary_text: format!(
            "主题 {} / 录音模式 {} / 设备 {}",
            cfg.general.theme,
            cfg.audio.recording_mode,
            selected_audio_device_name(&cfg, &audio_devices)
        ),
        live_session_text: format_live_session(&runtime),
        output_preview_text: format_output_preview(&runtime),
        overlay_state_text: format_overlay_state(&runtime),
        overlay_body_text: format_overlay_body(&cfg, &runtime),
    })
}

pub fn refresh_settings_window() -> Result<SettingsWindowSnapshot, String> {
    let paths = resolve_app_paths();
    config::init_config(&paths).map_err(|e| e.to_string())?;
    let cfg = config::get_config(&paths).map_err(|e| e.to_string())?;
    let runtime = state::get_runtime_snapshot();
    let live_model = active_live_model_info(&paths, &cfg);
    let refine_model = refine_model_info(&paths, &cfg);
    let live_model_downloaded = live_model
        .as_ref()
        .map(|model| model.is_downloaded)
        .unwrap_or(false);
    let refine_model_downloaded = refine_model
        .as_ref()
        .map(|model| model.is_downloaded)
        .unwrap_or(false);
    let live_model_downloading = crate::model_downloader::is_downloading(&cfg.asr.live_model_id);
    let refine_model_downloading =
        crate::model_downloader::is_downloading(&cfg.asr.refine_model_id);

    Ok(SettingsWindowSnapshot {
        theme_text: format!("主题: {}", cfg.general.theme),
        model_text: format!(
            "实时模型: {} ({})\n整体纠正: {} / {}",
            cfg.asr.live_model_id,
            model_install_state_text(live_model_downloaded, live_model_downloading),
            if cfg.asr.refine_enabled {
                "enabled"
            } else {
                "disabled"
            },
            format!(
                "{} ({})",
                cfg.asr.refine_model_id,
                model_install_state_text(refine_model_downloaded, refine_model_downloading)
            )
        ),
        audio_device_text: format!(
            "音频输入: {}",
            selected_audio_device_name(&cfg, &audio::list_input_devices().unwrap_or_default())
        ),
        recording_mode_text: format!("录音模式: {}", cfg.audio.recording_mode),
        punct_style_text: format!("标点风格: {}", cfg.output.punct_style),
        append_content_text: format!("附加内容: {}", cfg.output.append_content),
        hotword_summary_text: load_hotword_summary(&paths),
        hotkey_summary_text: load_hotkey_summary(&cfg),
        config_path_text: format!("配置路径: {}", paths.config_file().display()),
        llm_enabled: cfg.rewrite.enabled,
        llm_base_url: cfg.rewrite.providers.first()
            .map(|p| p.base_url.clone())
            .unwrap_or_default(),
        llm_model_name: cfg.rewrite.providers.first()
            .map(|p| p.model.clone())
            .unwrap_or_default(),
        llm_system_prompt: {
            let active_id = &cfg.rewrite.active_prompt_id;
            cfg.rewrite.prompts.iter()
                .find(|p| &p.id == active_id)
                .map(|p| p.system_prompt.clone())
                .unwrap_or_default()
        },
        refine_asr_enabled: cfg.asr.refine_enabled,
        overlay_enabled: runtime.overlay_visible,
    })
}

pub fn cycle_theme() -> Result<UiSnapshot, String> {
    let paths = resolve_app_paths();
    config::init_config(&paths).map_err(|e| e.to_string())?;
    let mut cfg = config::get_config(&paths).map_err(|e| e.to_string())?;
    cfg.general.theme = next_theme(&cfg.general.theme).to_string();
    config::save_config(&paths, &cfg).map_err(|e| e.to_string())?;
    refresh_snapshot()
}

pub fn cycle_recording_mode() -> Result<UiSnapshot, String> {
    let paths = resolve_app_paths();
    config::init_config(&paths).map_err(|e| e.to_string())?;
    let mut cfg = config::get_config(&paths).map_err(|e| e.to_string())?;
    cfg.audio.recording_mode = next_recording_mode(&cfg.audio.recording_mode).to_string();
    config::save_config(&paths, &cfg).map_err(|e| e.to_string())?;
    refresh_snapshot()
}

pub fn toggle_rewrite_enabled() -> Result<UiSnapshot, String> {
    let paths = resolve_app_paths();
    config::init_config(&paths).map_err(|e| e.to_string())?;
    let mut cfg = config::get_config(&paths).map_err(|e| e.to_string())?;
    cfg.rewrite.enabled = !cfg.rewrite.enabled;
    config::save_config(&paths, &cfg).map_err(|e| e.to_string())?;
    refresh_snapshot()
}

pub fn cycle_punct_style() -> Result<UiSnapshot, String> {
    let paths = resolve_app_paths();
    config::init_config(&paths).map_err(|e| e.to_string())?;
    let mut cfg = config::get_config(&paths).map_err(|e| e.to_string())?;
    cfg.output.punct_style = next_punct_style(&cfg.output.punct_style).to_string();
    config::save_config(&paths, &cfg).map_err(|e| e.to_string())?;
    refresh_snapshot()
}

pub fn cycle_append_content() -> Result<UiSnapshot, String> {
    let paths = resolve_app_paths();
    config::init_config(&paths).map_err(|e| e.to_string())?;
    let mut cfg = config::get_config(&paths).map_err(|e| e.to_string())?;
    cfg.output.append_content = next_append_content(&cfg.output.append_content).to_string();
    config::save_config(&paths, &cfg).map_err(|e| e.to_string())?;
    refresh_snapshot()
}

pub fn cycle_audio_device() -> Result<UiSnapshot, String> {
    let paths = resolve_app_paths();
    config::init_config(&paths).map_err(|e| e.to_string())?;
    let mut cfg = config::get_config(&paths).map_err(|e| e.to_string())?;
    let devices = audio::list_input_devices().map_err(|e| e.to_string())?;
    if devices.is_empty() {
        return Err("No audio input devices available".to_string());
    }

    let current_name = cfg.audio.device_name.clone().or_else(|| {
        devices
            .iter()
            .find(|device| device.is_default)
            .map(|device| device.name.clone())
    });

    let current_idx = current_name
        .as_ref()
        .and_then(|name| devices.iter().position(|device| device.name == *name))
        .unwrap_or(0);
    let next_idx = (current_idx + 1) % devices.len();
    cfg.audio.device_name = Some(devices[next_idx].name.clone());
    config::save_config(&paths, &cfg).map_err(|e| e.to_string())?;

    let mut snapshot = refresh_snapshot()?;
    snapshot.status_text = format!("Selected audio input {}", devices[next_idx].name);
    Ok(snapshot)
}

pub fn cycle_github_proxy() -> Result<UiSnapshot, String> {
    let paths = resolve_app_paths();
    config::init_config(&paths).map_err(|e| e.to_string())?;
    let mut cfg = config::get_config(&paths).map_err(|e| e.to_string())?;
    cfg.network.github_proxy = next_github_proxy(&cfg.network.github_proxy).to_string();
    config::save_config(&paths, &cfg).map_err(|e| e.to_string())?;

    let mut snapshot = refresh_snapshot()?;
    snapshot.status_text = format!("已切换下载代理为 {}", github_proxy_display(&cfg));
    Ok(snapshot)
}

pub fn set_github_proxy_index(index: i32) -> Result<UiSnapshot, String> {
    let paths = resolve_app_paths();
    config::init_config(&paths).map_err(|e| e.to_string())?;
    let mut cfg = config::get_config(&paths).map_err(|e| e.to_string())?;
    cfg.network.github_proxy = github_proxy_value(index).to_string();
    config::save_config(&paths, &cfg).map_err(|e| e.to_string())?;

    let mut snapshot = refresh_snapshot()?;
    snapshot.status_text = format!("已切换下载代理为 {}", github_proxy_display(&cfg));
    Ok(snapshot)
}

pub fn cycle_active_model() -> Result<UiSnapshot, String> {
    let paths = resolve_app_paths();
    config::init_config(&paths).map_err(|e| e.to_string())?;
    let mut cfg = config::get_config(&paths).map_err(|e| e.to_string())?;
    let models = model::list_models_with_paths(&paths).map_err(|e| e.to_string())?;

    let available: Vec<_> = models
        .into_iter()
        .filter(|m| m.backend == shanji_core::model::ModelBackend::Streaming)
        .collect();
    if available.is_empty() {
        return Err("No streaming models available in registry".to_string());
    }

    let preferred: Vec<_> = available.iter().filter(|m| m.is_downloaded).collect();
    let source: Vec<_> = if preferred.is_empty() {
        available.iter().collect()
    } else {
        preferred
    };

    let current_idx = source
        .iter()
        .position(|m| m.id == cfg.asr.live_model_id)
        .unwrap_or(0);
    let next_idx = (current_idx + 1) % source.len();
    cfg.asr.live_model_id = source[next_idx].id.clone();

    config::save_config(&paths, &cfg).map_err(|e| e.to_string())?;
    refresh_snapshot()
}

pub fn cycle_refine_model() -> Result<UiSnapshot, String> {
    let paths = resolve_app_paths();
    config::init_config(&paths).map_err(|e| e.to_string())?;
    let mut cfg = config::get_config(&paths).map_err(|e| e.to_string())?;
    let models = model::list_models_with_paths(&paths).map_err(|e| e.to_string())?;

    let available: Vec<_> = models
        .into_iter()
        .filter(|m| m.backend == shanji_core::model::ModelBackend::Whole)
        .collect();
    if available.is_empty() {
        return Err("No whole models available in registry".to_string());
    }

    let preferred: Vec<_> = available.iter().filter(|m| m.is_downloaded).collect();
    let source: Vec<_> = if preferred.is_empty() {
        available.iter().collect()
    } else {
        preferred
    };

    let current_idx = source
        .iter()
        .position(|m| m.id == cfg.asr.refine_model_id)
        .unwrap_or(0);
    let next_idx = (current_idx + 1) % source.len();
    cfg.asr.refine_model_id = source[next_idx].id.clone();

    config::save_config(&paths, &cfg).map_err(|e| e.to_string())?;
    refresh_snapshot()
}

pub fn toggle_refine_asr() -> Result<UiSnapshot, String> {
    let paths = resolve_app_paths();
    config::init_config(&paths).map_err(|e| e.to_string())?;
    let mut cfg = config::get_config(&paths).map_err(|e| e.to_string())?;
    cfg.asr.refine_enabled = !cfg.asr.refine_enabled;
    config::save_config(&paths, &cfg).map_err(|e| e.to_string())?;
    refresh_snapshot()
}

pub fn toggle_latest_hotword_library() -> Result<UiSnapshot, String> {
    let paths = resolve_app_paths();
    let libraries = hotwords::list_libraries_with_paths(&paths).map_err(|e| e.to_string())?;
    let library = libraries
        .first()
        .ok_or_else(|| "No hotword libraries available".to_string())?;

    hotwords::set_library_enabled_with_paths(&paths, &library.id, !library.enabled)
        .map_err(|e| e.to_string())?;

    let mut snapshot = refresh_snapshot()?;
    snapshot.status_text = format!(
        "Hotword library {} is now {}",
        library.name,
        if library.enabled {
            "disabled"
        } else {
            "enabled"
        }
    );
    Ok(snapshot)
}

pub fn clear_history() -> Result<UiSnapshot, String> {
    let paths = resolve_app_paths();
    let db = HistoryDb::new_with_paths(&paths).map_err(|e| e.to_string())?;
    let removed = db.clear_all().map_err(|e| e.to_string())?;

    let mut snapshot = refresh_snapshot()?;
    snapshot.status_text = format!("Cleared {} history records from shared core", removed);
    Ok(snapshot)
}

pub fn load_history_cards() -> Result<(Vec<shanji_core::history::HistoryCardData>, u32), String> {
    let paths = resolve_app_paths();
    let db = shanji_core::history::HistoryDb::new_with_paths(&paths)
        .map_err(|e| e.to_string())?;
    db.list_cards(20).map_err(|e| e.to_string())
}

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
    let cfg = shanji_core::config::get_config(&paths).map_err(|e| e.to_string())?;
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

pub fn set_llm_base_url(url: String) -> Result<(), String> {
    let paths = resolve_app_paths();
    shanji_core::config::init_config(&paths).map_err(|e| e.to_string())?;
    let mut cfg = shanji_core::config::get_config(&paths).map_err(|e| e.to_string())?;
    let provider = cfg.rewrite.providers.first_mut()
        .ok_or_else(|| "No LLM provider configured".to_string())?;
    provider.base_url = url;
    shanji_core::config::save_config(&paths, &cfg).map_err(|e| e.to_string())
}

pub fn set_llm_model_name(model: String) -> Result<(), String> {
    let paths = resolve_app_paths();
    shanji_core::config::init_config(&paths).map_err(|e| e.to_string())?;
    let mut cfg = shanji_core::config::get_config(&paths).map_err(|e| e.to_string())?;
    let provider = cfg.rewrite.providers.first_mut()
        .ok_or_else(|| "No LLM provider configured".to_string())?;
    provider.model = model;
    shanji_core::config::save_config(&paths, &cfg).map_err(|e| e.to_string())
}

pub fn set_llm_api_key(key: String) -> Result<(), String> {
    let paths = resolve_app_paths();
    let cfg = shanji_core::config::get_config(&paths).map_err(|e| e.to_string())?;
    let provider_id = cfg.rewrite.providers.first()
        .map(|p| p.id.clone())
        .unwrap_or_else(|| cfg.rewrite.active_provider_id.clone());
    if provider_id.is_empty() {
        return Err("No LLM provider configured".to_string());
    }
    shanji_core::llm::save_api_key(&provider_id, &key).map_err(|e| e.to_string())
}

pub fn set_llm_system_prompt(prompt: String) -> Result<(), String> {
    let paths = resolve_app_paths();
    shanji_core::config::init_config(&paths).map_err(|e| e.to_string())?;
    let mut cfg = shanji_core::config::get_config(&paths).map_err(|e| e.to_string())?;
    // system_prompt 存在于 prompts 列表中，修改当前激活的 prompt preset
    let active_id = cfg.rewrite.active_prompt_id.clone();
    // First try to find the active non-builtin preset
    if let Some(preset) = cfg.rewrite.prompts.iter_mut()
        .find(|p| p.id == active_id && !p.is_builtin)
    {
        preset.system_prompt = prompt;
    } else if let Some(preset) = cfg.rewrite.prompts.iter_mut()
        .find(|p| p.id == "custom")
    {
        // Fall back to any existing "custom" entry (even if not currently active)
        preset.system_prompt = prompt;
        cfg.rewrite.active_prompt_id = "custom".to_string();
    } else {
        // Create a new "custom" preset
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

pub fn start_model_download() -> Result<UiSnapshot, String> {
    start_live_model_download()
}

pub fn start_live_model_download() -> Result<UiSnapshot, String> {
    let paths = resolve_app_paths();
    config::init_config(&paths).map_err(|e| e.to_string())?;
    let cfg = config::get_config(&paths).map_err(|e| e.to_string())?;
    crate::model_downloader::start(paths, cfg.asr.live_model_id.clone())?;
    let mut snapshot = refresh_snapshot()?;
    snapshot.status_text = format!("开始下载实时模型 {}", cfg.asr.live_model_id);
    Ok(snapshot)
}

pub fn start_refine_model_download() -> Result<UiSnapshot, String> {
    let paths = resolve_app_paths();
    config::init_config(&paths).map_err(|e| e.to_string())?;
    let cfg = config::get_config(&paths).map_err(|e| e.to_string())?;
    crate::model_downloader::start(paths, cfg.asr.refine_model_id.clone())?;
    let mut snapshot = refresh_snapshot()?;
    snapshot.status_text = format!("开始下载整体纠正模型 {}", cfg.asr.refine_model_id);
    Ok(snapshot)
}

pub fn request_mic_permission() {
    shanji_core::audio::open_mic_permission_settings();
}

pub fn toggle_mic_monitor() -> Result<UiSnapshot, String> {
    let paths = resolve_app_paths();
    config::init_config(&paths).map_err(|e| e.to_string())?;
    let cfg = config::get_config(&paths).map_err(|e| e.to_string())?;

    if !crate::audio_monitor::is_running() && crate::audio_transcriber::is_running() {
        let _ = crate::audio_transcriber::stop();
    }

    let started = crate::audio_monitor::toggle(cfg.audio.device_name.clone())?;

    let mut snapshot = refresh_snapshot()?;
    snapshot.status_text = if started {
        "Native microphone monitor started".to_string()
    } else {
        "Native microphone monitor stopped".to_string()
    };
    Ok(snapshot)
}

pub fn toggle_live_asr() -> Result<UiSnapshot, String> {
    let paths = resolve_app_paths();
    config::init_config(&paths).map_err(|e| e.to_string())?;

    if !crate::audio_transcriber::is_running() && crate::audio_monitor::is_running() {
        let _ = crate::audio_monitor::stop();
    }

    let started = crate::audio_transcriber::toggle(paths)?;

    let mut snapshot = refresh_snapshot()?;
    snapshot.status_text = if started {
        "Native live ASR started".to_string()
    } else {
        "Native live ASR stopped".to_string()
    };
    Ok(snapshot)
}

pub fn clear_session() -> Result<UiSnapshot, String> {
    if crate::audio_monitor::is_running() {
        let _ = crate::audio_monitor::stop();
    }
    if crate::audio_transcriber::is_running() {
        let _ = crate::audio_transcriber::stop();
    }

    state::reset_runtime();
    state::set_status_message("Native session state cleared");

    let mut snapshot = refresh_snapshot()?;
    snapshot.status_text = "Native session state cleared".to_string();
    Ok(snapshot)
}

pub fn toggle_overlay_visibility() -> Result<UiSnapshot, String> {
    let runtime = state::get_runtime_snapshot();
    state::set_overlay_visible(!runtime.overlay_visible);
    state::set_status_message(if runtime.overlay_visible {
        "Overlay hidden from native app"
    } else {
        "Overlay shown from native app"
    });
    refresh_snapshot()
}

fn resolve_app_paths() -> AppPaths {
    shanji_core::paths::standard_app_paths("shanji").expect("failed to resolve standard app paths")
}

fn format_state(state: AppState) -> String {
    let label = match state {
        AppState::Idle => "Idle",
        AppState::Recording => "Recording",
        AppState::Transcribing => "Transcribing",
        AppState::Rewriting => "Rewriting",
    };

    format!("App state: {}", label)
}

fn format_overlay_state(runtime: &RuntimeSnapshot) -> String {
    match runtime.current_state {
        AppState::Idle => "Overlay idle".to_string(),
        AppState::Recording => "Recording".to_string(),
        AppState::Transcribing => "Transcribing".to_string(),
        AppState::Rewriting => "Rewriting".to_string(),
    }
}

fn format_overlay_body(cfg: &AppConfig, runtime: &RuntimeSnapshot) -> String {
    match runtime.current_state {
        AppState::Idle => {
            if !runtime.final_output.is_empty() {
                runtime.final_output.clone()
            } else {
                format!("Ready with {}", cfg.asr.live_model_id)
            }
        }
        AppState::Recording => {
            if !runtime.live_transcript.is_empty() {
                runtime.live_transcript.clone()
            } else {
                "正在聆听...".to_string()
            }
        }
        AppState::Transcribing => {
            if !runtime.live_transcript.is_empty() {
                runtime.live_transcript.clone()
            } else {
                "正在识别...".to_string()
            }
        }
        AppState::Rewriting => runtime.rewrite_preview.clone(),
    }
}

fn next_theme(current: &str) -> &'static str {
    match current {
        "system" => "light",
        "light" => "dark",
        "dark" => "system",
        _ => "system",
    }
}

fn load_audio_device_summary(cfg: &AppConfig) -> String {
    let Ok(devices) = audio::list_input_devices() else {
        return "Audio devices unavailable".to_string();
    };

    let selected = cfg
        .audio
        .device_name
        .clone()
        .or_else(|| {
            devices
                .iter()
                .find(|device| device.is_default)
                .map(|device| device.name.clone())
        })
        .unwrap_or_else(|| "system default".to_string());

    format!(
        "Audio input: {} devices / selected {} / monitor {} / live asr {}",
        devices.len(),
        selected,
        if state::is_mic_test_running() {
            "on"
        } else {
            "off"
        },
        if crate::audio_transcriber::is_running() {
            "on"
        } else {
            "off"
        },
    )
}

fn load_live_asr_summary(cfg: &AppConfig, runtime: &RuntimeSnapshot) -> String {
    let source = if runtime.final_output.is_empty() {
        runtime.last_transcript.as_str()
    } else {
        runtime.final_output.as_str()
    };

    if crate::audio_transcriber::is_running()
        || matches!(
            runtime.current_state,
            AppState::Recording | AppState::Transcribing
        )
    {
        return format!(
            "Live ASR: running with {}{}",
            cfg.asr.live_model_id,
            if cfg.asr.refine_enabled {
                " + whole-model refine"
            } else {
                ""
            }
        );
    }

    if matches!(runtime.current_state, AppState::Rewriting) {
        return "Live ASR: rewriting final transcript".to_string();
    }

    if source.is_empty() {
        format!(
            "Live ASR: ready with {}{}",
            cfg.asr.live_model_id,
            if cfg.asr.refine_enabled {
                " / refine enabled"
            } else {
                ""
            }
        )
    } else {
        format!("Live ASR: last output {}", truncate(source, 26))
    }
}

fn load_hotkey_summary(cfg: &AppConfig) -> String {
    hotkeys::summarize_hotkeys(&cfg.hotkeys).status_line()
}

fn load_tray_summary(cfg: &AppConfig, runtime: &RuntimeSnapshot) -> String {
    tray::build_tray_menu(
        "Shanji",
        runtime.current_state.clone(),
        cfg.general.minimize_to_tray,
    )
    .status_line()
}

fn load_hotkey_inventory(cfg: &AppConfig) -> String {
    hotkeys::summarize_hotkeys(&cfg.hotkeys).inventory_text()
}

fn load_tray_inventory(cfg: &AppConfig, runtime: &RuntimeSnapshot) -> String {
    tray::build_tray_menu(
        "Shanji",
        runtime.current_state.clone(),
        cfg.general.minimize_to_tray,
    )
    .inventory_text()
}

fn active_live_model_info(
    paths: &AppPaths,
    cfg: &AppConfig,
) -> Option<shanji_core::model::ModelInfo> {
    model::list_models_with_paths(paths)
        .ok()
        .and_then(|models| {
            models
                .into_iter()
                .find(|entry| entry.id == cfg.asr.live_model_id)
        })
}

fn refine_model_info(paths: &AppPaths, cfg: &AppConfig) -> Option<shanji_core::model::ModelInfo> {
    model::list_models_with_paths(paths)
        .ok()
        .and_then(|models| {
            models
                .into_iter()
                .find(|entry| entry.id == cfg.asr.refine_model_id)
        })
}

fn model_install_state_text(downloaded: bool, downloading: bool) -> &'static str {
    if downloading {
        "downloading"
    } else if downloaded {
        "downloaded"
    } else {
        "remote"
    }
}

fn refine_runtime_summary(cfg: &AppConfig, downloaded: bool, downloading: bool) -> String {
    if !cfg.asr.refine_enabled {
        return format!("未启用 · {}", cfg.asr.refine_model_id);
    }

    if downloading {
        return format!("已启用，正在下载 {}", cfg.asr.refine_model_id);
    }

    if downloaded {
        return format!("已启用 · {}", cfg.asr.refine_model_id);
    }

    format!("已启用但未安装 · {}", cfg.asr.refine_model_id)
}

fn selected_audio_device_name(
    cfg: &AppConfig,
    devices: &[shanji_core::audio::AudioInputDevice],
) -> String {
    cfg.audio
        .device_name
        .clone()
        .or_else(|| {
            devices
                .iter()
                .find(|device| device.is_default)
                .map(|device| device.name.clone())
        })
        .unwrap_or_else(|| "system default".to_string())
}

fn human_readable_bytes(bytes: u64) -> String {
    const GB: u64 = 1024 * 1024 * 1024;
    const MB: u64 = 1024 * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}

fn next_recording_mode(current: &str) -> &'static str {
    match current {
        "toggle" => "push-to-talk",
        "push-to-talk" => "toggle",
        _ => "toggle",
    }
}

fn next_punct_style(current: &str) -> &'static str {
    match current {
        "zh" => "en",
        "en" => "none",
        "none" => "zh",
        _ => "zh",
    }
}

fn next_append_content(current: &str) -> &'static str {
    match current {
        "space" => "newline",
        "newline" => "none",
        "none" => "space",
        _ => "space",
    }
}

fn next_github_proxy(current: &str) -> &'static str {
    github_proxy_value(github_proxy_index_from_value(current) + 1)
}

fn github_proxy_index(cfg: &AppConfig) -> i32 {
    github_proxy_index_from_value(&cfg.network.github_proxy)
}

fn github_proxy_index_from_value(value: &str) -> i32 {
    GITHUB_PROXY_VALUES
        .iter()
        .position(|candidate| candidate == &value.trim())
        .map(|index| index as i32)
        .unwrap_or(0)
}

fn github_proxy_value(index: i32) -> &'static str {
    let normalized = index.rem_euclid(GITHUB_PROXY_VALUES.len() as i32) as usize;
    GITHUB_PROXY_VALUES[normalized]
}

fn load_history_preview(paths: &AppPaths) -> String {
    let Ok(db) = HistoryDb::new_with_paths(paths) else {
        return "History preview unavailable".to_string();
    };

    match db.list(0, 5) {
        Ok(records) if !records.is_empty() => records
            .into_iter()
            .enumerate()
            .map(|(idx, record)| format!("{}. {}", idx + 1, truncate(&record.transcribed, 44)))
            .collect::<Vec<_>>()
            .join("\n"),
        Ok(_) => "No history records yet".to_string(),
        Err(_) => "History preview unavailable".to_string(),
    }
}

fn record_output_text(record: &shanji_core::history::HistoryRecord) -> String {
    record
        .rewritten
        .clone()
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| record.transcribed.clone())
}

fn load_history_stats(paths: &AppPaths) -> String {
    let Ok(db) = HistoryDb::new_with_paths(paths) else {
        return "History: unavailable".to_string();
    };

    let total = match db.count() {
        Ok(total) => total,
        Err(_) => return "History: unavailable".to_string(),
    };
    let latest = db
        .list(0, 1)
        .ok()
        .and_then(|records| records.into_iter().next());

    match latest {
        Some(record) => format!(
            "History: {} total / latest {}{}",
            total,
            truncate(&record.transcribed, 28),
            if record.audio_path.is_some() {
                " / audio"
            } else {
                ""
            }
        ),
        None => format!("History: {} total / no entries yet", total),
    }
}

fn open_path_in_system(path: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut cmd = std::process::Command::new("open");
        cmd.arg(path);
        cmd
    };

    #[cfg(target_os = "linux")]
    let mut command = {
        let mut cmd = std::process::Command::new("xdg-open");
        cmd.arg(path);
        cmd
    };

    #[cfg(target_os = "windows")]
    let mut command = {
        let mut cmd = std::process::Command::new("cmd");
        cmd.args(["/C", "start", "", path]);
        cmd
    };

    command
        .spawn()
        .map_err(|e| format!("Failed to open {}: {}", path, e))?;
    Ok(())
}

fn load_hotword_summary(paths: &AppPaths) -> String {
    let Ok(libraries) = hotwords::list_libraries_with_paths(paths) else {
        return "Hotwords unavailable".to_string();
    };

    if libraries.is_empty() {
        return "Hotwords: no libraries".to_string();
    }

    let enabled_count = libraries.iter().filter(|lib| lib.enabled).count();
    let total_words: usize = libraries
        .iter()
        .filter(|lib| lib.enabled)
        .map(|lib| lib.word_count)
        .sum();

    let names = libraries
        .iter()
        .filter(|lib| lib.enabled)
        .take(3)
        .map(|lib| lib.name.clone())
        .collect::<Vec<_>>();

    if enabled_count == 0 {
        "Hotwords: libraries exist but none enabled".to_string()
    } else {
        format!(
            "Hotwords: {} enabled / {} words{}",
            enabled_count,
            total_words,
            if names.is_empty() {
                String::new()
            } else {
                format!(" ({})", names.join(", "))
            }
        )
    }
}

fn load_hotword_inventory(paths: &AppPaths) -> String {
    let Ok(libraries) = hotwords::list_libraries_with_paths(paths) else {
        return "Hotword inventory unavailable".to_string();
    };

    if libraries.is_empty() {
        return "No hotword libraries yet".to_string();
    }

    libraries
        .into_iter()
        .take(4)
        .map(|library| {
            format!(
                "{} {} · {} words",
                if library.enabled { "On" } else { "Off" },
                library.name,
                library.word_count
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn github_proxy_display(cfg: &AppConfig) -> &str {
    let value = cfg.network.github_proxy.trim();
    if value.is_empty() {
        "直连 GitHub"
    } else {
        value
    }
}

fn load_model_summary(paths: &AppPaths, cfg: &AppConfig) -> String {
    let Ok(models) = model::list_models_with_paths(paths) else {
        return "Models unavailable".to_string();
    };

    let downloaded = models.iter().filter(|m| m.is_downloaded).count();
    let live_name = models
        .iter()
        .find(|m| m.id == cfg.asr.live_model_id)
        .map(|m| m.name.clone())
        .unwrap_or_else(|| cfg.asr.live_model_id.clone());
    let refine_name = models
        .iter()
        .find(|m| m.id == cfg.asr.refine_model_id)
        .map(|m| m.name.clone())
        .unwrap_or_else(|| cfg.asr.refine_model_id.clone());

    format!(
        "Models: {} downloaded / live {} / refine {} ({})",
        downloaded,
        live_name,
        refine_name,
        if cfg.asr.refine_enabled {
            "enabled"
        } else {
            "disabled"
        }
    )
}

fn load_model_inventory(paths: &AppPaths, cfg: &AppConfig) -> String {
    let Ok(models) = model::list_models_with_paths(paths) else {
        return "Model inventory unavailable".to_string();
    };

    if models.is_empty() {
        return "No models available in registry".to_string();
    }

    models
        .into_iter()
        .take(5)
        .map(|entry| {
            let role = if entry.id == cfg.asr.live_model_id {
                "live"
            } else if entry.id == cfg.asr.refine_model_id {
                "refine"
            } else {
                "idle"
            };
            let installed = if entry.is_downloaded {
                "downloaded"
            } else {
                "remote"
            };
            format!("{} · {} · {}", entry.name, role, installed)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_live_session(runtime: &RuntimeSnapshot) -> String {
    match runtime.current_state {
        AppState::Idle => {
            if runtime.last_transcript.is_empty() {
                "Runtime idle. Start `Live ASR` for real recognition or `Mic Monitor` for level feedback."
                    .to_string()
            } else {
                format!(
                    "Last transcript\n{}\n\nMic level: {}%",
                    runtime.last_transcript,
                    (runtime.audio_level * 100.0).round() as u32
                )
            }
        }
        AppState::Recording => format!(
            "Recording session started\nMic level: {}%\nAwaiting partial transcript...",
            (runtime.audio_level * 100.0).round() as u32
        ),
        AppState::Transcribing => format!(
            "Live transcript\n{}\n\nMic level: {}%",
            runtime.live_transcript,
            (runtime.audio_level * 100.0).round() as u32
        ),
        AppState::Rewriting => format!(
            "Original\n{}\n\nRewrite preview\n{}",
            if runtime.last_transcript.is_empty() {
                runtime.live_transcript.as_str()
            } else {
                runtime.last_transcript.as_str()
            },
            runtime.rewrite_preview
        ),
    }
}

fn format_output_preview(runtime: &RuntimeSnapshot) -> String {
    if !runtime.final_output.is_empty() {
        return format!("Final output\n{}", runtime.final_output);
    }

    if !runtime.rewrite_preview.is_empty() {
        return format!("Pending output\n{}", runtime.rewrite_preview);
    }

    if !runtime.live_transcript.is_empty() {
        return format!("ASR preview\n{}", runtime.live_transcript);
    }

    "No output generated yet".to_string()
}

fn truncate(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{}...", truncated)
    } else {
        truncated
    }
}


#[allow(dead_code)]
fn _config_for_future_use(paths: &AppPaths) -> Result<AppConfig, String> {
    config::get_config(paths).map_err(|e| e.to_string())
}

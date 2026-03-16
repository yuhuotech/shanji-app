mod app;
mod audio_monitor;
mod audio_transcriber;
mod model_downloader;
mod platform_runtime;

slint::include_modules!();

const OVERLAY_BOTTOM_OFFSET_RATIO: f32 = 0.07;

fn init_logging() {
    let env = env_logger::Env::default().default_filter_or("info,ort=warn,reqwest=warn");
    let mut builder = env_logger::Builder::from_env(env);
    builder.format_timestamp_millis();
    let _ = builder.try_init();
}

#[cfg(target_os = "macos")]
fn overlay_target_position(window: &slint::Window) -> Option<slint::LogicalPosition> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSScreen;

    let mtm = MainThreadMarker::new()?;
    let screen = NSScreen::mainScreen(mtm)?;
    let visible_frame = screen.visibleFrame();
    let main_screen_height = screen.frame().size.height;
    let window_size = window.size().to_logical(window.scale_factor());

    let x = visible_frame.origin.x
        + ((visible_frame.size.width - window_size.width as f64) / 2.0).max(0.0);
    let bottom_offset = visible_frame.size.height * OVERLAY_BOTTOM_OFFSET_RATIO as f64;
    let y = main_screen_height - visible_frame.origin.y - bottom_offset - window_size.height as f64;

    Some(slint::LogicalPosition::new(x as f32, y as f32))
}

#[cfg(not(target_os = "macos"))]
fn overlay_target_position(_window: &slint::Window) -> Option<slint::LogicalPosition> {
    None
}

fn position_overlay_window(overlay: &OverlayWindow) {
    let window = overlay.window();
    if let Some(position) = overlay_target_position(&window) {
        window.set_position(position);
    }
}

fn main() -> Result<(), slint::PlatformError> {
    init_logging();
    let app = AppWindow::new()?;
    let settings = SettingsWindow::new()?;
    let overlay = OverlayWindow::new()?;
    let platform_runtime = std::rc::Rc::new(std::cell::RefCell::new(
        platform_runtime::PlatformRuntime::new(),
    ));

    apply_snapshot(&app, &overlay, app::bootstrap_snapshot());
    refresh_settings_from_app(&settings);

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    app.on_refresh_requested(move || {
        if let (Some(app), Some(overlay)) = (weak.upgrade(), weak_overlay.upgrade()) {
            apply_result(&app, &overlay, app::refresh_snapshot());
        }
    });

    let weak_settings = settings.as_weak();
    app.on_open_history_requested(move || {
        if let Some(settings) = weak_settings.upgrade() {
            settings.set_active_section(SettingsSection::History);
            let _ = settings.show();
            refresh_settings_from_app(&settings);
        }
    });

    let weak_settings = settings.as_weak();
    app.on_open_settings_requested(move || {
        if let Some(settings) = weak_settings.upgrade() {
            let _ = settings.show();
            apply_settings_snapshot(&settings, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    app.on_download_model_requested(move || {
        if let (Some(app), Some(overlay)) = (weak.upgrade(), weak_overlay.upgrade()) {
            apply_result(&app, &overlay, app::start_model_download());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    app.on_github_proxy_selected(move |index| {
        if let (Some(app), Some(overlay)) = (weak.upgrade(), weak_overlay.upgrade()) {
            apply_result(&app, &overlay, app::set_github_proxy_index(index));
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    app.on_toggle_mic_monitor_requested(move || {
        if let (Some(app), Some(overlay)) = (weak.upgrade(), weak_overlay.upgrade()) {
            apply_result(&app, &overlay, app::toggle_mic_monitor());
        }
    });

    app.on_request_mic_permission(move || {
        app::request_mic_permission();
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    app.on_toggle_live_asr_requested(move || {
        if let (Some(app), Some(overlay)) = (weak.upgrade(), weak_overlay.upgrade()) {
            apply_result(&app, &overlay, app::toggle_live_asr());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    let weak_settings = settings.as_weak();
    settings.on_cycle_theme_requested(move || {
        if let (Some(app), Some(overlay), Some(settings)) = (
            weak.upgrade(),
            weak_overlay.upgrade(),
            weak_settings.upgrade(),
        ) {
            apply_result(&app, &overlay, app::cycle_theme());
            apply_settings_snapshot(&settings, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    let weak_settings = settings.as_weak();
    settings.on_cycle_github_proxy_requested(move || {
        if let (Some(app), Some(overlay), Some(settings)) = (
            weak.upgrade(),
            weak_overlay.upgrade(),
            weak_settings.upgrade(),
        ) {
            apply_result(&app, &overlay, app::cycle_github_proxy());
            apply_settings_snapshot(&settings, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    let weak_settings = settings.as_weak();
    settings.on_cycle_live_model_requested(move || {
        if let (Some(app), Some(overlay), Some(settings)) = (
            weak.upgrade(),
            weak_overlay.upgrade(),
            weak_settings.upgrade(),
        ) {
            apply_result(&app, &overlay, app::cycle_active_model());
            apply_settings_snapshot(&settings, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    let weak_settings = settings.as_weak();
    settings.on_cycle_refine_model_requested(move || {
        if let (Some(app), Some(overlay), Some(settings)) = (
            weak.upgrade(),
            weak_overlay.upgrade(),
            weak_settings.upgrade(),
        ) {
            apply_result(&app, &overlay, app::cycle_refine_model());
            apply_settings_snapshot(&settings, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    let weak_settings = settings.as_weak();
    settings.on_toggle_refine_asr_requested(move || {
        if let (Some(app), Some(overlay), Some(settings)) = (
            weak.upgrade(),
            weak_overlay.upgrade(),
            weak_settings.upgrade(),
        ) {
            apply_result(&app, &overlay, app::toggle_refine_asr());
            apply_settings_snapshot(&settings, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    let weak_settings = settings.as_weak();
    settings.on_download_live_model_requested(move || {
        if let (Some(app), Some(overlay), Some(settings)) = (
            weak.upgrade(),
            weak_overlay.upgrade(),
            weak_settings.upgrade(),
        ) {
            apply_result(&app, &overlay, app::start_live_model_download());
            apply_settings_snapshot(&settings, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    let weak_settings = settings.as_weak();
    settings.on_download_refine_model_requested(move || {
        if let (Some(app), Some(overlay), Some(settings)) = (
            weak.upgrade(),
            weak_overlay.upgrade(),
            weak_settings.upgrade(),
        ) {
            apply_result(&app, &overlay, app::start_refine_model_download());
            apply_settings_snapshot(&settings, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    let weak_settings = settings.as_weak();
    settings.on_cycle_audio_device_requested(move || {
        if let (Some(app), Some(overlay), Some(settings)) = (
            weak.upgrade(),
            weak_overlay.upgrade(),
            weak_settings.upgrade(),
        ) {
            apply_result(&app, &overlay, app::cycle_audio_device());
            apply_settings_snapshot(&settings, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    let weak_settings = settings.as_weak();
    settings.on_cycle_recording_mode_requested(move || {
        if let (Some(app), Some(overlay), Some(settings)) = (
            weak.upgrade(),
            weak_overlay.upgrade(),
            weak_settings.upgrade(),
        ) {
            apply_result(&app, &overlay, app::cycle_recording_mode());
            apply_settings_snapshot(&settings, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    let weak_settings = settings.as_weak();
    settings.on_toggle_rewrite_requested(move || {
        if let (Some(app), Some(overlay), Some(settings)) = (
            weak.upgrade(),
            weak_overlay.upgrade(),
            weak_settings.upgrade(),
        ) {
            apply_result(&app, &overlay, app::toggle_rewrite_enabled());
            apply_settings_snapshot(&settings, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    let weak_settings = settings.as_weak();
    settings.on_cycle_punct_style_requested(move || {
        if let (Some(app), Some(overlay), Some(settings)) = (
            weak.upgrade(),
            weak_overlay.upgrade(),
            weak_settings.upgrade(),
        ) {
            apply_result(&app, &overlay, app::cycle_punct_style());
            apply_settings_snapshot(&settings, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    let weak_settings = settings.as_weak();
    settings.on_cycle_append_content_requested(move || {
        if let (Some(app), Some(overlay), Some(settings)) = (
            weak.upgrade(),
            weak_overlay.upgrade(),
            weak_settings.upgrade(),
        ) {
            apply_result(&app, &overlay, app::cycle_append_content());
            apply_settings_snapshot(&settings, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    let weak_settings = settings.as_weak();
    settings.on_toggle_hotword_requested(move || {
        if let (Some(app), Some(overlay), Some(settings)) = (
            weak.upgrade(),
            weak_overlay.upgrade(),
            weak_settings.upgrade(),
        ) {
            apply_result(&app, &overlay, app::toggle_latest_hotword_library());
            apply_settings_snapshot(&settings, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    let weak_settings = settings.as_weak();
    settings.on_toggle_overlay_requested(move || {
        if let (Some(app), Some(overlay), Some(settings)) = (
            weak.upgrade(),
            weak_overlay.upgrade(),
            weak_settings.upgrade(),
        ) {
            apply_result(&app, &overlay, app::toggle_overlay_visibility());
            apply_settings_snapshot(&settings, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    let weak_settings = settings.as_weak();
    settings.on_clear_session_requested(move || {
        if let (Some(app), Some(overlay), Some(settings)) = (
            weak.upgrade(),
            weak_overlay.upgrade(),
            weak_settings.upgrade(),
        ) {
            apply_result(&app, &overlay, app::clear_session());
            apply_settings_snapshot(&settings, app::refresh_settings_window());
        }
    });

    // ── History callbacks ──────────────────────────────────────────────────

    let weak_settings = settings.as_weak();
    settings.on_refresh_history_requested(move || {
        if let Some(s) = weak_settings.upgrade() {
            refresh_settings_from_app(&s);
        }
    });

    let weak_settings = settings.as_weak();
    settings.on_clear_history_requested(move || {
        if let Some(s) = weak_settings.upgrade() {
            let _ = app::clear_history();
            refresh_settings_from_app(&s);
        }
    });

    settings.on_copy_record_requested(move |id| {
        let _ = app::copy_history_record(id);
    });

    let weak_settings = settings.as_weak();
    settings.on_paste_record_requested(move |id| {
        let _ = app::paste_history_record(id);
        if let Some(s) = weak_settings.upgrade() {
            refresh_settings_from_app(&s);
        }
    });

    let weak_settings = settings.as_weak();
    settings.on_delete_record_requested(move |id| {
        let _ = app::delete_history_record(id);
        if let Some(s) = weak_settings.upgrade() {
            refresh_settings_from_app(&s);
        }
    });

    settings.on_play_audio_requested(move |id| {
        let _ = app::play_history_audio(id);
    });

    settings.on_retranscribe_requested(move |id| {
        let _ = app::retranscribe_history(id);
    });

    // ── LLM config callbacks ───────────────────────────────────────────────

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

    // ── Timers ─────────────────────────────────────────────────────────────

    let bootstrap_timer = slint::Timer::default();
    let platform_runtime_ref = platform_runtime.clone();
    bootstrap_timer.start(
        slint::TimerMode::SingleShot,
        std::time::Duration::from_millis(0),
        move || {
            let mut runtime = platform_runtime_ref.borrow_mut();
            let _ = runtime.sync();
        },
    );

    let platform_timer = slint::Timer::default();
    let weak = app.as_weak();
    let weak_settings = settings.as_weak();
    let weak_overlay = overlay.as_weak();
    let platform_runtime_ref = platform_runtime.clone();
    platform_timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(32),
        move || {
            if let (Some(app), Some(settings), Some(overlay)) = (
                weak.upgrade(),
                weak_settings.upgrade(),
                weak_overlay.upgrade(),
            ) {
                let hotkey_actions = {
                    let runtime = platform_runtime_ref.borrow();
                    runtime.poll_hotkey_events()
                };
                for action in hotkey_actions {
                    handle_platform_hotkey_event(&app, &settings, &overlay, action);
                }
                let tray_actions = {
                    let runtime = platform_runtime_ref.borrow();
                    runtime.poll_tray_events()
                };
                for action in tray_actions {
                    handle_platform_tray_event(&app, &settings, &overlay, action);
                }
            }
        },
    );

    let refresh_timer = slint::Timer::default();
    let weak = app.as_weak();
    let weak_settings = settings.as_weak();
    let weak_overlay = overlay.as_weak();
    let platform_runtime_ref = platform_runtime.clone();
    refresh_timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(450),
        move || {
            if let (Some(app), Some(settings), Some(overlay)) = (
                weak.upgrade(),
                weak_settings.upgrade(),
                weak_overlay.upgrade(),
            ) {
                {
                    let mut runtime = platform_runtime_ref.borrow_mut();
                    let _ = runtime.sync();
                }
                if let Ok(snapshot) = app::refresh_snapshot() {
                    apply_snapshot(&app, &overlay, snapshot);
                }
                if settings.window().is_visible() {
                    refresh_settings_from_app(&settings);
                }
            }
        },
    );

    app.show()?;
    slint::run_event_loop_until_quit()
}

fn handle_platform_hotkey_event(
    app: &AppWindow,
    settings: &SettingsWindow,
    overlay: &OverlayWindow,
    event: shanji_platform::hotkeys::PlatformHotkeyEvent,
) {
    use shanji_platform::hotkeys::{HotkeyAction, HotkeyEventState};

    match (event.action, event.state) {
        (HotkeyAction::ToggleRecording, HotkeyEventState::Pressed) => {
            apply_result(app, overlay, app::toggle_live_asr());
        }
        (HotkeyAction::PushToTalk, HotkeyEventState::Pressed) => {
            let paths = shanji_core::paths::standard_app_paths("shanji")
                .map_err(|err| err.to_string())
                .and_then(|paths| {
                    if crate::audio_monitor::is_running() {
                        let _ = crate::audio_monitor::stop();
                    }
                    crate::audio_transcriber::start(paths)
                })
                .and_then(|_| app::refresh_snapshot());
            apply_result(app, overlay, paths);
        }
        (HotkeyAction::PushToTalk, HotkeyEventState::Released) => {
            let result = crate::audio_transcriber::stop()
                .map(|_| ())
                .map_err(|err| err.to_string())
                .and_then(|_| app::refresh_snapshot());
            apply_result(app, overlay, result);
        }
        (HotkeyAction::OpenMainWindow, HotkeyEventState::Pressed) => {
            let _ = app.show();
            apply_result(app, overlay, app::refresh_snapshot());
        }
        (HotkeyAction::OpenHistory, HotkeyEventState::Pressed) => {
            settings.set_active_section(SettingsSection::History);
            let _ = settings.show();
            refresh_settings_from_app(settings);
        }
        (HotkeyAction::ToggleRewrite, HotkeyEventState::Pressed) => {
            apply_result(app, overlay, app::toggle_rewrite_enabled());
            apply_settings_snapshot(settings, app::refresh_settings_window());
        }
        _ => {}
    }
}

fn handle_platform_tray_event(
    app: &AppWindow,
    settings: &SettingsWindow,
    overlay: &OverlayWindow,
    event: shanji_platform::tray::PlatformTrayEvent,
) {
    use shanji_platform::tray::{PlatformTrayEvent, TrayAction};

    match event {
        PlatformTrayEvent::PrimaryClick | PlatformTrayEvent::Action(TrayAction::ShowMainWindow) => {
            let _ = app.show();
            apply_result(app, overlay, app::refresh_snapshot());
        }
        PlatformTrayEvent::Action(TrayAction::ToggleRecording) => {
            apply_result(app, overlay, app::toggle_live_asr());
        }
        PlatformTrayEvent::Action(TrayAction::OpenHistory) => {
            settings.set_active_section(SettingsSection::History);
            let _ = settings.show();
            refresh_settings_from_app(settings);
        }
        PlatformTrayEvent::Action(TrayAction::OpenSettings) => {
            let _ = settings.show();
            apply_settings_snapshot(settings, app::refresh_settings_window());
        }
        PlatformTrayEvent::Action(TrayAction::Quit) => {
            let _ = overlay.hide();
            let _ = settings.hide();
            let _ = app.hide();
            let _ = slint::quit_event_loop();
        }
    }
}

fn apply_result(app: &AppWindow, overlay: &OverlayWindow, result: Result<app::UiSnapshot, String>) {
    match result {
        Ok(snapshot) => apply_snapshot(app, overlay, snapshot),
        Err(err) => app.set_status_text(format!("Action failed: {}", err).into()),
    }
}

fn apply_snapshot(app: &AppWindow, overlay: &OverlayWindow, snapshot: app::UiSnapshot) {
    app.set_app_name(snapshot.app_name.into());
    app.set_app_subtitle_text(snapshot.app_subtitle_text.into());
    app.set_status_text(snapshot.status_text.into());
    app.set_hotkey_hint_title(snapshot.hotkey_hint_title.into());
    app.set_hotkey_hint_body(snapshot.hotkey_hint_body.into());
    app.set_hotkey_display_text(snapshot.hotkey_display_text.into());
    app.set_model_title_text(snapshot.model_title_text.into());
    app.set_model_desc_text(snapshot.model_desc_text.into());
    app.set_model_version_text(snapshot.model_version_text.into());
    app.set_model_size_text(snapshot.model_size_text.into());
    app.set_model_language_text(snapshot.model_language_text.into());
    app.set_model_install_text(snapshot.model_install_text.into());
    app.set_model_ready(snapshot.model_ready);
    app.set_model_downloading(snapshot.model_downloading);
    app.set_model_download_progress(snapshot.model_download_progress);
    app.set_model_download_status_text(snapshot.model_download_status_text.into());
    app.set_model_download_error_text(snapshot.model_download_error_text.into());
    app.set_github_proxy_current_index(snapshot.github_proxy_index);
    app.set_mic_permission_title(snapshot.mic_permission_title.into());
    app.set_mic_permission_body(snapshot.mic_permission_body.into());
    app.set_mic_ready(snapshot.mic_ready);
    app.set_paste_permission_title(snapshot.paste_permission_title.into());
    app.set_paste_permission_body(snapshot.paste_permission_body.into());
    app.set_paste_ready(snapshot.paste_ready);
    app.set_default_device_text(snapshot.default_device_text.into());
    app.set_transcribe_card_title(snapshot.transcribe_card_title.into());
    app.set_transcribe_button_text(snapshot.transcribe_button_text.into());
    app.set_transcribe_body_text(snapshot.transcribe_body_text.into());
    app.set_audio_level_text(snapshot.audio_level_text.into());
    app.set_audio_level_value(snapshot.audio_level_value);
    app.set_monitor_button_text(snapshot.monitor_button_text.into());
    app.set_monitor_tip_text(snapshot.monitor_tip_text.into());
    app.set_rewrite_text(snapshot.rewrite_text.into());
    app.set_tray_summary_text(snapshot.tray_summary_text.into());
    app.set_live_asr_text(snapshot.live_asr_text.into());
    app.set_history_stats_text(snapshot.history_stats_text.into());
    app.set_history_preview_text(snapshot.history_preview_text.into());
    app.set_settings_summary_text(snapshot.settings_summary_text.into());
    overlay.set_overlay_state_text(snapshot.overlay_state_text.into());
    overlay.set_overlay_audio_level(snapshot.audio_level_value);
    overlay.set_overlay_live_text(snapshot.overlay_body_text.into());

    if snapshot.overlay_visible {
        position_overlay_window(overlay);
        let _ = overlay.show();
    } else {
        let _ = overlay.hide();
    }
}

fn apply_settings_snapshot(
    settings: &SettingsWindow,
    result: Result<app::SettingsWindowSnapshot, String>,
) {
    match result {
        Ok(snapshot) => {
            settings.set_theme_text(snapshot.theme_text.into());
            settings.set_model_text(snapshot.model_text.into());
            settings.set_audio_device_text(snapshot.audio_device_text.into());
            settings.set_recording_mode_text(snapshot.recording_mode_text.into());
            settings.set_punct_style_text(snapshot.punct_style_text.into());
            settings.set_append_content_text(snapshot.append_content_text.into());
            settings.set_hotword_summary_text(snapshot.hotword_summary_text.into());
            settings.set_hotkey_summary_text(snapshot.hotkey_summary_text.into());
            settings.set_config_path_text(snapshot.config_path_text.into());
            settings.set_llm_enabled(snapshot.llm_enabled);
            settings.set_llm_base_url(snapshot.llm_base_url.into());
            settings.set_llm_model_name(snapshot.llm_model_name.into());
            settings.set_llm_system_prompt(snapshot.llm_system_prompt.into());
            settings.set_refine_asr_enabled(snapshot.refine_asr_enabled);
            settings.set_overlay_visible(snapshot.overlay_enabled);
        }
        Err(_err) => {
            // status display handled elsewhere in new settings design
        }
    }
}

fn refresh_settings_from_app(settings: &SettingsWindow) {
    if let Ok(snapshot) = app::refresh_settings_window() {
        apply_settings_snapshot(settings, Ok(snapshot));
    }
    // Load history cards
    match app::load_history_cards() {
        Ok((cards, total)) => {
            let slint_cards: Vec<HistoryCardData> = cards
                .into_iter()
                .map(|c| HistoryCardData {
                    record_id: c.record_id,
                    timestamp: c.timestamp.into(),
                    text: c.text.into(),
                    has_audio: c.has_audio,
                    is_llm_rewritten: c.is_llm_rewritten,
                    was_pasted: c.was_pasted,
                })
                .collect();
            settings.set_history_records(
                std::rc::Rc::new(slint::VecModel::from(slint_cards)).into(),
            );
            settings.set_history_stats_text(format!("共 {} 条 · 最近 20 条", total).into());
        }
        Err(e) => log::warn!("Failed to load history cards: {}", e),
    }
}

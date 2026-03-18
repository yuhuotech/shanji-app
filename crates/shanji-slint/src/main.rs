mod app;
mod audio_monitor;
mod audio_transcriber;
mod model_downloader;
mod platform_runtime;

slint::include_modules!();

const OVERLAY_BOTTOM_OFFSET_RATIO: f32 = 0.15;
const OVERLAY_CAPSULE_HEIGHT: f32 = 34.0;

thread_local! {
    static PLATFORM_RUNTIME_SLOT: std::cell::RefCell<Option<std::rc::Rc<std::cell::RefCell<platform_runtime::PlatformRuntime>>>> =
        const { std::cell::RefCell::new(None) };
}

fn init_logging() {
    let env = env_logger::Env::default().default_filter_or("info,ort=warn,reqwest=warn");
    let mut builder = env_logger::Builder::from_env(env);
    builder.format_timestamp_millis();
    let _ = builder.try_init();
}

#[cfg(target_os = "macos")]
fn set_dock_icon() {
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("set_dock_icon: not on main thread");
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let png_bytes = include_bytes!("../assets/shanji_logo.png");
    log::info!("set_dock_icon: {} bytes embedded", png_bytes.len());
    let data = NSData::with_bytes(png_bytes);
    match NSImage::initWithData(NSImage::alloc(), &data) {
        Some(image) => {
            unsafe { app.setApplicationIconImage(Some(&image)) };
            log::info!("set_dock_icon: icon applied");
        }
        None => {
            log::warn!("set_dock_icon: NSImage::initWithData returned None");
        }
    }
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
    // Anchor the top capsule itself, not the full overlay height, so transcript text can
    // grow downward without pushing the capsule upward.
    let y =
        main_screen_height - visible_frame.origin.y - bottom_offset - OVERLAY_CAPSULE_HEIGHT as f64;

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

fn install_platform_runtime(
    runtime: std::rc::Rc<std::cell::RefCell<platform_runtime::PlatformRuntime>>,
) {
    PLATFORM_RUNTIME_SLOT.with(|slot| {
        *slot.borrow_mut() = Some(runtime);
    });
}

fn sync_platform_runtime() {
    PLATFORM_RUNTIME_SLOT.with(|slot| {
        if let Some(runtime) = slot.borrow().as_ref() {
            let _ = runtime.borrow_mut().sync();
        }
    });
}

fn is_settings_page_visible(app: &AppWindow) -> bool {
    app.get_show_settings_page() && app.window().is_visible()
}

fn show_settings_page(app: &AppWindow) {
    app.set_show_settings_page(true);
    let _ = app.show();
}

fn hide_settings_page(app: &AppWindow) {
    app.set_show_settings_page(false);
}

fn install_platform_event_handlers(
    app: slint::Weak<AppWindow>,
    overlay: slint::Weak<OverlayWindow>,
) {
    let hotkey_app = app.clone();
    let hotkey_overlay = overlay.clone();
    global_hotkey::GlobalHotKeyEvent::set_event_handler(Some(move |event| {
        let Some(action) = shanji_platform::hotkeys::translate_global_event(event) else {
            return;
        };
        let overlay = hotkey_overlay.clone();
        let _ = hotkey_app.upgrade_in_event_loop(move |app| {
            if let Some(overlay) = overlay.upgrade() {
                handle_platform_hotkey_event(&app, &overlay, action);
            }
        });
    }));

    let tray_app = app.clone();
    let tray_overlay = overlay.clone();
    tray_icon::menu::MenuEvent::set_event_handler(Some(move |event| {
        let Some(action) = shanji_platform::tray::translate_menu_event(event) else {
            return;
        };
        let overlay = tray_overlay.clone();
        let _ = tray_app.upgrade_in_event_loop(move |app| {
            if let Some(overlay) = overlay.upgrade() {
                handle_platform_tray_event(&app, &overlay, action);
            }
        });
    }));

    let tray_icon_app = app;
    let tray_icon_overlay = overlay;
    tray_icon::TrayIconEvent::set_event_handler(Some(move |event| {
        let Some(action) = shanji_platform::tray::translate_tray_icon_event(event) else {
            return;
        };
        let overlay = tray_icon_overlay.clone();
        let _ = tray_icon_app.upgrade_in_event_loop(move |app| {
            if let Some(overlay) = overlay.upgrade() {
                handle_platform_tray_event(&app, &overlay, action);
            }
        });
    }));
}

fn install_runtime_event_listener(
    app: slint::Weak<AppWindow>,
    overlay: slint::Weak<OverlayWindow>,
) {
    let rx = shanji_core::state::subscribe_runtime_events();
    std::thread::spawn(move || {
        while let Ok(first) = rx.recv() {
            let mut should_sync_platform =
                matches!(first, shanji_core::state::RuntimeEventKind::Platform);
            while let Ok(event) = rx.recv_timeout(std::time::Duration::from_millis(16)) {
                if matches!(event, shanji_core::state::RuntimeEventKind::Platform) {
                    should_sync_platform = true;
                }
            }

            let overlay = overlay.clone();
            let _ = app.upgrade_in_event_loop(move |app| {
                let Some(overlay) = overlay.upgrade() else {
                    return;
                };
                if should_sync_platform {
                    sync_platform_runtime();
                    if let Ok(snapshot) = app::refresh_snapshot() {
                        apply_snapshot(&app, &overlay, snapshot);
                    }
                    return;
                }
                if let Ok(snapshot) = app::refresh_runtime_ui() {
                    apply_runtime_snapshot(&app, &overlay, snapshot);
                }
            });
        }
    });
}

fn install_config_event_listener(app: slint::Weak<AppWindow>, overlay: slint::Weak<OverlayWindow>) {
    let rx = shanji_core::config::subscribe_config_events();
    std::thread::spawn(move || {
        while rx.recv().is_ok() {
            let overlay = overlay.clone();
            let _ = app.upgrade_in_event_loop(move |app| {
                sync_platform_runtime();
                if let Some(overlay) = overlay.upgrade() {
                    if let Ok(snapshot) = app::refresh_snapshot() {
                        apply_snapshot(&app, &overlay, snapshot);
                    }
                }
                if is_settings_page_visible(&app) {
                    apply_settings_snapshot(&app, app::refresh_settings_window());
                }
            });
        }
    });
}

fn install_download_event_listener(
    app: slint::Weak<AppWindow>,
    overlay: slint::Weak<OverlayWindow>,
) {
    let rx = model_downloader::subscribe_download_events();
    std::thread::spawn(move || {
        while rx.recv().is_ok() {
            while rx
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_ok()
            {}

            let overlay = overlay.clone();
            let _ = app.upgrade_in_event_loop(move |app| {
                if let Some(progress) = app::get_download_progress() {
                    if let Some(main_progress) = app::get_main_window_download_progress() {
                        apply_app_download_progress_only(&app, main_progress);
                    }
                    if is_settings_page_visible(&app) {
                        apply_download_progress_only(&app, progress);
                    }
                    return;
                }

                if let Some(overlay) = overlay.upgrade() {
                    if let Ok(snapshot) = app::refresh_snapshot() {
                        apply_snapshot(&app, &overlay, snapshot);
                    }
                }
                if is_settings_page_visible(&app) {
                    apply_settings_snapshot(&app, app::refresh_settings_window());
                }
            });
        }
    });
}

fn install_history_playback_listener(app: slint::Weak<AppWindow>) {
    let rx = app::subscribe_history_playback_events();
    std::thread::spawn(move || {
        while rx.recv().is_ok() {
            while rx
                .recv_timeout(std::time::Duration::from_millis(16))
                .is_ok()
            {}

            let _ = app.upgrade_in_event_loop(move |app| {
                if is_settings_page_visible(&app) {
                    refresh_history_only(&app);
                }
            });
        }
    });
}

fn main() -> Result<(), slint::PlatformError> {
    init_logging();
    let app = AppWindow::new()?;
    let overlay = OverlayWindow::new()?;
    let platform_runtime = std::rc::Rc::new(std::cell::RefCell::new(
        platform_runtime::PlatformRuntime::new(),
    ));
    install_platform_runtime(platform_runtime.clone());
    install_platform_event_handlers(app.as_weak(), overlay.as_weak());
    install_runtime_event_listener(app.as_weak(), overlay.as_weak());
    install_config_event_listener(app.as_weak(), overlay.as_weak());
    install_download_event_listener(app.as_weak(), overlay.as_weak());
    install_history_playback_listener(app.as_weak());

    apply_snapshot(&app, &overlay, app::bootstrap_snapshot());
    refresh_settings_from_app(&app);

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    app.on_refresh_requested(move || {
        if let (Some(app), Some(overlay)) = (weak.upgrade(), weak_overlay.upgrade()) {
            apply_result(&app, &overlay, app::refresh_snapshot());
        }
    });

    let weak = app.as_weak();
    app.on_open_history_requested(move || {
        if let Some(app) = weak.upgrade() {
            app.set_active_section(SettingsSection::History);
            show_settings_page(&app);
            refresh_settings_from_app(&app);
        }
    });

    let weak = app.as_weak();
    app.on_open_settings_requested(move || {
        if let Some(app) = weak.upgrade() {
            app.set_active_section(SettingsSection::Asr);
            show_settings_page(&app);
            refresh_settings_from_app(&app);
        }
    });

    let weak = app.as_weak();
    app.on_close_settings_requested(move || {
        if let Some(app) = weak.upgrade() {
            hide_settings_page(&app);
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
    app.on_cycle_theme_requested(move || {
        if let (Some(app), Some(overlay)) = (weak.upgrade(), weak_overlay.upgrade()) {
            apply_result(&app, &overlay, app::cycle_theme());
            apply_settings_snapshot(&app, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    app.on_cycle_github_proxy_requested(move || {
        if let (Some(app), Some(overlay)) = (weak.upgrade(), weak_overlay.upgrade()) {
            apply_result(&app, &overlay, app::cycle_github_proxy());
            apply_settings_snapshot(&app, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    app.on_toggle_refine_asr_requested(move || {
        if let (Some(app), Some(overlay)) = (weak.upgrade(), weak_overlay.upgrade()) {
            apply_result(&app, &overlay, app::toggle_refine_asr());
            apply_settings_snapshot(&app, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    app.on_download_live_model_requested(move || {
        if let (Some(app), Some(overlay)) = (weak.upgrade(), weak_overlay.upgrade()) {
            apply_result(&app, &overlay, app::start_live_model_download());
            apply_settings_snapshot(&app, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    app.on_download_refine_model_requested(move || {
        if let (Some(app), Some(overlay)) = (weak.upgrade(), weak_overlay.upgrade()) {
            apply_result(&app, &overlay, app::start_refine_model_download());
            apply_settings_snapshot(&app, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    app.on_select_audio_device_requested(move |index| {
        if let Some(app) = weak.upgrade() {
            apply_settings_snapshot(&app, app::select_audio_device(index as usize));
        }
    });

    let weak = app.as_weak();
    app.on_toggle_hotword_library_requested(move |id| {
        if let Some(app) = weak.upgrade() {
            apply_settings_snapshot(&app, app::toggle_hotword_library(&id));
        }
    });

    let weak = app.as_weak();
    app.on_open_hotword_dir_requested(move || {
        if let Some(app) = weak.upgrade() {
            apply_settings_snapshot(&app, app::open_hotword_directory());
        }
    });

    let weak = app.as_weak();
    app.on_test_mic_requested(move || {
        if let Some(app) = weak.upgrade() {
            app.set_mic_test_status("🔴 录音中… (功能待实现)".into());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    app.on_toggle_rewrite_requested(move || {
        if let (Some(app), Some(overlay)) = (weak.upgrade(), weak_overlay.upgrade()) {
            apply_result(&app, &overlay, app::toggle_rewrite_enabled());
            apply_settings_snapshot(&app, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    app.on_cycle_punct_style_requested(move || {
        if let (Some(app), Some(overlay)) = (weak.upgrade(), weak_overlay.upgrade()) {
            apply_result(&app, &overlay, app::cycle_punct_style());
            apply_settings_snapshot(&app, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    app.on_cycle_append_content_requested(move || {
        if let (Some(app), Some(overlay)) = (weak.upgrade(), weak_overlay.upgrade()) {
            apply_result(&app, &overlay, app::cycle_append_content());
            apply_settings_snapshot(&app, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    app.on_toggle_overlay_requested(move || {
        if let (Some(app), Some(overlay)) = (weak.upgrade(), weak_overlay.upgrade()) {
            apply_result(&app, &overlay, app::toggle_overlay_visibility());
            apply_settings_snapshot(&app, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    app.on_clear_session_requested(move || {
        if let (Some(app), Some(overlay)) = (weak.upgrade(), weak_overlay.upgrade()) {
            apply_result(&app, &overlay, app::clear_session());
            apply_settings_snapshot(&app, app::refresh_settings_window());
        }
    });

    // ── History callbacks ──────────────────────────────────────────────────

    let weak = app.as_weak();
    app.on_refresh_history_requested(move || {
        if let Some(app) = weak.upgrade() {
            refresh_history_only(&app);
        }
    });

    let weak = app.as_weak();
    app.on_clear_history_requested(move || {
        if let Some(app) = weak.upgrade() {
            let _ = app::clear_history();
            refresh_history_only(&app);
        }
    });

    app.on_copy_record_requested(move |id| {
        let _ = app::copy_history_record(id);
    });

    let weak = app.as_weak();
    app.on_paste_record_requested(move |id| {
        let _ = app::paste_history_record(id);
        if let Some(app) = weak.upgrade() {
            refresh_history_only(&app);
        }
    });

    let weak = app.as_weak();
    app.on_delete_record_requested(move |id| {
        let _ = app::delete_history_record(id);
        if let Some(app) = weak.upgrade() {
            refresh_history_only(&app);
        }
    });

    app.on_play_audio_requested(move |id| {
        let _ = app::play_history_audio(id);
    });

    app.on_retranscribe_requested(move |id| {
        let _ = app::retranscribe_history(id);
    });

    // ── LLM config callbacks ───────────────────────────────────────────────

    app.on_set_llm_base_url(move |url| {
        let _ = app::set_llm_base_url(url.to_string());
    });

    app.on_set_llm_api_key(move |key| {
        let _ = app::set_llm_api_key(key.to_string());
    });

    app.on_set_llm_model_name(move |model| {
        let _ = app::set_llm_model_name(model.to_string());
    });

    app.on_set_llm_system_prompt(move |prompt| {
        let _ = app::set_llm_system_prompt(prompt.to_string());
    });

    let weak = app.as_weak();
    app.on_test_llm_api_requested(move || {
        if let Some(app) = weak.upgrade() {
            app.set_llm_test_status_text("".into());
            app.set_llm_test_status_type(0);
            app.set_llm_testing(true);
            let weak2 = weak.clone();
            std::thread::spawn(move || {
                let result = app::test_llm_api();
                slint::invoke_from_event_loop(move || {
                    if let Some(app) = weak2.upgrade() {
                        app.set_llm_testing(false);
                        match result {
                            Ok(_) => {
                                app.set_llm_test_status_text("✓ 连接正常".into());
                                app.set_llm_test_status_type(1);
                            }
                            Err(e) => {
                                app.set_llm_test_status_text(format!("✗ {}", e).into());
                                app.set_llm_test_status_type(2);
                            }
                        }
                    }
                }).ok();
            });
        }
    });

    app.on_record_hotkey_requested(move || {
        // TODO: implement hotkey recording UI flow
        log::info!("Hotkey recording requested from settings");
    });

    // ── Timers ─────────────────────────────────────────────────────────────

    let bootstrap_timer = slint::Timer::default();
    bootstrap_timer.start(
        slint::TimerMode::SingleShot,
        std::time::Duration::from_millis(0),
        move || {
            sync_platform_runtime();
        },
    );

    #[cfg(target_os = "macos")]
    let native_hotkey_timer = slint::Timer::default();
    #[cfg(target_os = "macos")]
    {
        let weak = app.as_weak();
        let weak_overlay = overlay.as_weak();
        native_hotkey_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(16),
            move || {
                let events = shanji_platform::hotkeys::drain_native_events();
                if events.is_empty() {
                    return;
                }

                if let (Some(app), Some(overlay)) = (weak.upgrade(), weak_overlay.upgrade()) {
                    for event in events {
                        handle_platform_hotkey_event(&app, &overlay, event);
                    }
                }
            },
        );
    }

    app.show()?;

    // 在事件循环第一次迭代后再设置 Dock 图标，确保 NSApplication 完全初始化
    #[cfg(target_os = "macos")]
    let dock_icon_timer = slint::Timer::default();
    #[cfg(target_os = "macos")]
    dock_icon_timer.start(
        slint::TimerMode::SingleShot,
        std::time::Duration::from_millis(0),
        || set_dock_icon(),
    );

    slint::run_event_loop_until_quit()
}

fn handle_platform_hotkey_event(
    app: &AppWindow,
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
            app.set_active_section(SettingsSection::History);
            show_settings_page(app);
            refresh_settings_from_app(app);
        }
        (HotkeyAction::ToggleRewrite, HotkeyEventState::Pressed) => {
            apply_result(app, overlay, app::toggle_rewrite_enabled());
            apply_settings_snapshot(app, app::refresh_settings_window());
        }
        _ => {}
    }
}

fn handle_platform_tray_event(
    app: &AppWindow,
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
            app.set_active_section(SettingsSection::History);
            show_settings_page(app);
            refresh_settings_from_app(app);
        }
        PlatformTrayEvent::Action(TrayAction::OpenSettings) => {
            app.set_active_section(SettingsSection::Asr);
            show_settings_page(app);
            apply_settings_snapshot(app, app::refresh_settings_window());
        }
        PlatformTrayEvent::Action(TrayAction::Quit) => {
            let _ = overlay.hide();
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
    app.set_refine_model_title_text(snapshot.refine_model_title_text.into());
    app.set_refine_model_desc_text(snapshot.refine_model_desc_text.into());
    app.set_refine_model_version_text(snapshot.refine_model_version_text.into());
    app.set_refine_model_language_text(snapshot.refine_model_language_text.into());
    app.set_refine_asr_enabled(snapshot.refine_asr_enabled);
    app.set_refine_model_downloaded(snapshot.refine_model_ready);
    app.set_refine_model_downloading(snapshot.refine_model_downloading);
    app.set_refine_model_download_progress(snapshot.refine_model_download_progress);
    app.set_refine_model_download_status_text(snapshot.refine_model_download_status_text.into());
    app.set_refine_model_download_error_text(snapshot.refine_model_download_error_text.into());
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

fn apply_runtime_snapshot(
    app: &AppWindow,
    overlay: &OverlayWindow,
    snapshot: app::RuntimeUiSnapshot,
) {
    app.set_status_text(snapshot.status_text.into());
    app.set_transcribe_button_text(snapshot.transcribe_button_text.into());
    app.set_transcribe_body_text(snapshot.transcribe_body_text.into());
    app.set_audio_level_text(snapshot.audio_level_text.into());
    app.set_audio_level_value(snapshot.audio_level_value);
    app.set_monitor_button_text(snapshot.monitor_button_text.into());
    app.set_tray_summary_text(snapshot.tray_summary_text.into());
    app.set_live_asr_text(snapshot.live_asr_text.into());
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

fn apply_app_download_progress_only(app: &AppWindow, snapshot: app::MainWindowDownloadSnapshot) {
    app.set_model_ready(snapshot.model_ready);
    app.set_model_downloading(snapshot.model_downloading);
    app.set_model_download_progress(snapshot.model_download_progress);
    app.set_model_download_status_text(snapshot.model_download_status_text.into());
    app.set_model_download_error_text(snapshot.model_download_error_text.into());
    app.set_refine_model_downloaded(snapshot.refine_model_ready);
    app.set_refine_model_downloading(snapshot.refine_model_downloading);
    app.set_refine_model_download_progress(snapshot.refine_model_download_progress);
    app.set_refine_model_download_status_text(snapshot.refine_model_download_status_text.into());
    app.set_refine_model_download_error_text(snapshot.refine_model_download_error_text.into());
}

fn apply_settings_snapshot(
    settings: &AppWindow,
    result: Result<app::SettingsWindowSnapshot, String>,
) {
    match result {
        Ok(snapshot) => {
            settings.set_theme_text(snapshot.theme_text.into());
            settings.set_punct_style_text(snapshot.punct_style_text.into());
            settings.set_append_content_text(snapshot.append_content_text.into());
            settings.set_hotkey_summary_text(snapshot.hotkey_summary_text.into());
            settings.set_config_path_text(snapshot.config_path_text.into());
            settings.set_llm_enabled(snapshot.llm_enabled);
            settings.set_llm_base_url(snapshot.llm_base_url.into());
            settings.set_llm_api_key_saved(snapshot.llm_api_key_saved);
            settings.set_llm_model_name(snapshot.llm_model_name.into());
            settings.set_llm_system_prompt(snapshot.llm_system_prompt.into());
            settings.set_llm_test_status_type(snapshot.llm_test_status);
            settings.set_llm_test_status_text(snapshot.llm_test_status_text.into());
            settings.set_overlay_visible(snapshot.overlay_enabled);
            // live model
            settings.set_live_model_id(snapshot.live_model_id.into());
            settings.set_live_model_size_text(snapshot.live_model_size_text.into());
            settings.set_live_model_downloaded(snapshot.live_model_downloaded);
            settings.set_live_model_downloading(snapshot.live_model_downloading);
            settings.set_live_model_download_progress(snapshot.live_model_download_progress);
            settings.set_live_model_download_status_text(
                snapshot.live_model_download_status_text.into(),
            );
            // refine model
            settings.set_refine_asr_enabled(snapshot.refine_asr_enabled);
            settings.set_refine_model_id(snapshot.refine_model_id.into());
            settings.set_refine_model_size_text(snapshot.refine_model_size_text.into());
            settings.set_refine_model_downloaded(snapshot.refine_model_downloaded);
            settings.set_refine_model_downloading(snapshot.refine_model_downloading);
            settings.set_refine_model_download_progress(snapshot.refine_model_download_progress);
            settings.set_refine_model_download_status_text(
                snapshot.refine_model_download_status_text.into(),
            );
            // hotwords
            settings.set_hotword_stats_text(snapshot.hotword_stats_text.into());
            settings.set_hotword_dir_text(snapshot.hotword_dir_text.into());
            settings.set_hotword_hint_text(snapshot.hotword_hint_text.into());
            let slint_libs: Vec<HotwordLibraryCard> = snapshot
                .hotword_libraries
                .into_iter()
                .map(|l| HotwordLibraryCard {
                    id: l.id.into(),
                    name: l.name.into(),
                    enabled: l.enabled,
                    word_count_text: l.word_count_text.into(),
                    is_builtin: l.is_builtin,
                    source_text: l.source_text.into(),
                    format_text: l.format_text.into(),
                })
                .collect();
            settings
                .set_hotword_libraries(std::rc::Rc::new(slint::VecModel::from(slint_libs)).into());
            // audio
            let slint_devices: Vec<slint::SharedString> = snapshot
                .audio_devices
                .into_iter()
                .map(|s| s.into())
                .collect();
            settings
                .set_audio_devices(std::rc::Rc::new(slint::VecModel::from(slint_devices)).into());
            settings.set_audio_device_index(snapshot.audio_device_index);
        }
        Err(_err) => {
            // status display handled elsewhere in new settings design
        }
    }
}

fn apply_download_progress_only(settings: &AppWindow, snap: app::DownloadProgressSnapshot) {
    settings.set_live_model_downloading(snap.live_model_downloading);
    settings.set_live_model_download_progress(snap.live_model_download_progress);
    settings.set_live_model_download_status_text(snap.live_model_download_status_text.into());
    settings.set_refine_model_downloading(snap.refine_model_downloading);
    settings.set_refine_model_download_progress(snap.refine_model_download_progress);
    settings.set_refine_model_download_status_text(snap.refine_model_download_status_text.into());
}

fn refresh_settings_from_app(settings: &AppWindow) {
    // 设置窗口打开时刷新设备缓存（CoreAudio 枚举只在此处发生，不在轮询定时器里）
    app::refresh_audio_device_cache();
    if let Ok(snapshot) = app::refresh_settings_window() {
        apply_settings_snapshot(settings, Ok(snapshot));
    }
    refresh_history_only(settings);
}

fn refresh_history_only(settings: &AppWindow) {
    match app::load_history_cards() {
        Ok((cards, total)) => {
            let active_record_id = app::active_history_playback_record_id();
            let slint_cards: Vec<HistoryCardData> = cards
                .into_iter()
                .map(|c| HistoryCardData {
                    record_id: c.record_id,
                    timestamp: c.timestamp.into(),
                    text: c.text.into(),
                    has_audio: c.has_audio,
                    is_playing: active_record_id == Some(c.record_id),
                    is_llm_rewritten: c.is_llm_rewritten,
                    was_pasted: c.was_pasted,
                })
                .collect();
            settings
                .set_history_records(std::rc::Rc::new(slint::VecModel::from(slint_cards)).into());
            settings.set_history_stats_text(format!("共 {} 条 · 最近 20 条", total).into());
        }
        Err(e) => log::warn!("Failed to load history cards: {}", e),
    }
}

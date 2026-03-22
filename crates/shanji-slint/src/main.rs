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
    static HOTKEY_CAPTURE_SLOT: std::cell::RefCell<HotkeyCaptureState> =
        const {
            std::cell::RefCell::new(HotkeyCaptureState {
                pending_shortcut: None,
                #[cfg(target_os = "macos")]
                fn_pressed: false,
            })
        };
}

#[derive(Default)]
struct HotkeyCaptureState {
    pending_shortcut: Option<String>,
    #[cfg(target_os = "macos")]
    fn_pressed: bool,
}

#[derive(Clone, Copy, Default, Debug)]
struct CapturedModifiers {
    alt: bool,
    control: bool,
    shift: bool,
    meta: bool,
}

fn init_logging() {
    let env = env_logger::Env::default().default_filter_or("info,ort=warn,reqwest=warn");
    let mut builder = env_logger::Builder::from_env(env);
    builder.format_timestamp_millis();
    if let Some(log_file) = open_log_file() {
        builder.target(env_logger::Target::Pipe(Box::new(log_file)));
    }
    let _ = builder.try_init();
}

fn open_log_file() -> Option<std::fs::File> {
    let log_dir = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)?
        .join("Library/Logs/Shanji");
    std::fs::create_dir_all(&log_dir).ok()?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("shanji.log"))
        .ok()
}

#[cfg(target_os = "macos")]
fn show_startup_error_dialog(message: &str) {
    let escaped = message.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        "display alert \"闪记启动失败\" message \"{}\" as critical",
        escaped
    );
    let _ = std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .status();
}

#[cfg(not(target_os = "macos"))]
fn show_startup_error_dialog(_message: &str) {}

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

fn reset_hotkey_capture_state() {
    HOTKEY_CAPTURE_SLOT.with(|slot| {
        let mut state = slot.borrow_mut();
        state.pending_shortcut = None;
        #[cfg(target_os = "macos")]
        {
            state.fn_pressed = false;
        }
    });
}

fn set_pending_hotkey_capture(shortcut: String) {
    HOTKEY_CAPTURE_SLOT.with(|slot| {
        slot.borrow_mut().pending_shortcut = Some(shortcut);
    });
}

fn take_pending_hotkey_capture() -> Option<String> {
    HOTKEY_CAPTURE_SLOT.with(|slot| slot.borrow_mut().pending_shortcut.take())
}

fn take_pending_hotkey_capture_matching(released_tokens: &[String]) -> Option<String> {
    HOTKEY_CAPTURE_SLOT.with(|slot| {
        let mut state = slot.borrow_mut();
        let should_take = state
            .pending_shortcut
            .as_ref()
            .map(|shortcut| {
                shortcut
                    .split('+')
                    .any(|part| released_tokens.iter().any(|token| token == part))
            })
            .unwrap_or(false);
        if should_take {
            state.pending_shortcut.take()
        } else {
            None
        }
    })
}

#[cfg(target_os = "macos")]
fn update_native_fn_capture_state(is_pressed: bool) -> Option<bool> {
    HOTKEY_CAPTURE_SLOT.with(|slot| {
        let mut state = slot.borrow_mut();
        match (state.fn_pressed, is_pressed) {
            (false, true) => {
                state.fn_pressed = true;
                Some(true)
            }
            (true, false) => {
                state.fn_pressed = false;
                Some(false)
            }
            _ => None,
        }
    })
}

fn hotkey_capture_recording_help_text() -> &'static str {
    "录制中：按下目标快捷键；如果只设置修饰键，松开后保存；Esc 取消"
}

fn hotkey_capture_pending_help_text(shortcut: &str) -> String {
    format!(
        "已识别：{}。松开后保存",
        app::format_hotkey_display(shortcut)
    )
}

fn hotkey_capture_saved_help_text(shortcut: &str) -> String {
    format!(
        "已保存：{}。新的快捷键已立即全局生效",
        app::format_hotkey_display(shortcut)
    )
}

fn hotkey_capture_cancelled_help_text(current_hotkey: &str) -> String {
    format!("已取消，本次未修改；当前仍为 {}", current_hotkey)
}

fn hotkey_capture_unchanged_help_text(shortcut: &str) -> String {
    format!(
        "快捷键没有变化；当前仍为 {}",
        app::format_hotkey_display(shortcut)
    )
}

fn finish_hotkey_capture_unchanged(app: &AppWindow, shortcut: &str) {
    reset_hotkey_capture_state();
    app.set_hotkey_recording(false);
    app.set_status_text("快捷键没有变化，无需保存".into());
    app.set_hotkey_help_text(hotkey_capture_unchanged_help_text(shortcut).into());
    log::info!("hotkey capture unchanged: shortcut={}", shortcut);
}

fn begin_hotkey_capture(app: &AppWindow) {
    reset_hotkey_capture_state();
    app.set_hotkey_recording(true);
    app.set_hotkey_help_text(hotkey_capture_recording_help_text().into());
    log::info!(
        "hotkey capture started: current_hotkey={}",
        app.get_hotkey_current_text()
    );
}

fn cancel_hotkey_capture(app: &AppWindow) {
    reset_hotkey_capture_state();
    app.set_hotkey_recording(false);
    app.set_hotkey_help_text(app::hotkey_settings_help_text().into());
    log::info!("hotkey capture canceled");
}

fn handle_hotkey_capture_pressed(
    app: &AppWindow,
    overlay: &OverlayWindow,
    text: &str,
    modifiers: CapturedModifiers,
    repeat: bool,
) {
    if !app.get_hotkey_recording() || repeat {
        return;
    }

    if is_slint_key(text, slint::platform::Key::Escape) {
        cancel_hotkey_capture(app);
        return;
    }

    let Some(shortcut) = shortcut_from_captured_key(text, modifiers) else {
        log::info!(
            "hotkey capture press ignored: text={:?}, modifiers={:?}, repeat={}",
            text,
            modifiers,
            repeat
        );
        return;
    };

    log::info!(
        "hotkey capture press parsed: text={:?}, modifiers={:?}, repeat={}, shortcut={}",
        text,
        modifiers,
        repeat,
        shortcut
    );

    match app::is_push_to_talk_hotkey_unchanged(&shortcut) {
        Ok(true) => {
            finish_hotkey_capture_unchanged(app, &shortcut);
            return;
        }
        Ok(false) => {}
        Err(err) => {
            log::warn!(
                "hotkey capture unchanged check failed: shortcut={}, err={}",
                shortcut,
                err
            );
        }
    }

    if shortcut_is_modifier_only(&shortcut) {
        set_pending_hotkey_capture(shortcut.clone());
        app.set_hotkey_help_text(hotkey_capture_pending_help_text(&shortcut).into());
        log::info!(
            "hotkey capture pending modifier-only shortcut: {}",
            shortcut
        );
        return;
    }

    reset_hotkey_capture_state();
    commit_hotkey_capture(app, overlay, shortcut);
}

fn handle_hotkey_capture_released(
    app: &AppWindow,
    overlay: &OverlayWindow,
    text: &str,
    modifiers: CapturedModifiers,
) {
    if !app.get_hotkey_recording() {
        return;
    }

    let Some(shortcut) = take_pending_hotkey_capture_for_release(text, modifiers) else {
        return;
    };

    commit_hotkey_capture(app, overlay, shortcut);
}

#[cfg(target_os = "macos")]
fn poll_macos_native_hotkey_capture(app: &AppWindow, overlay: &OverlayWindow) {
    if !app.get_hotkey_recording() {
        let _ = update_native_fn_capture_state(
            shanji_platform::hotkeys::is_native_modifier_pressed("Fn"),
        );
        return;
    }

    let is_pressed = shanji_platform::hotkeys::is_native_modifier_pressed("Fn");

    match update_native_fn_capture_state(is_pressed) {
        Some(true) => {
            let shortcut = "Fn";
            match app::is_push_to_talk_hotkey_unchanged(shortcut) {
                Ok(true) => {
                    finish_hotkey_capture_unchanged(app, shortcut);
                    return;
                }
                Ok(false) => {}
                Err(err) => {
                    log::warn!(
                        "native hotkey capture unchanged check failed: shortcut={}, err={}",
                        shortcut,
                        err
                    );
                }
            }

            set_pending_hotkey_capture(shortcut.to_string());
            app.set_hotkey_help_text(hotkey_capture_pending_help_text(shortcut).into());
            log::info!(
                "native hotkey capture pending modifier-only shortcut: {}",
                shortcut
            );
        }
        Some(false) => {
            let shortcut = take_pending_hotkey_capture_matching(&["Fn".to_string()])
                .or_else(take_pending_hotkey_capture);
            if let Some(shortcut) = shortcut {
                log::info!(
                    "native hotkey capture release committed shortcut={}",
                    shortcut
                );
                commit_hotkey_capture(app, overlay, shortcut);
            }
        }
        None => {}
    }
}

fn commit_hotkey_capture(app: &AppWindow, overlay: &OverlayWindow, shortcut: String) {
    let saved_help_text = hotkey_capture_saved_help_text(&shortcut);
    log::info!("hotkey capture commit requested: shortcut={}", shortcut);
    match app::set_push_to_talk_hotkey(shortcut) {
        Ok(snapshot) => {
            cancel_hotkey_capture(app);
            apply_snapshot(app, overlay, snapshot);
            apply_settings_snapshot(app, app::refresh_settings_window());
            app.set_hotkey_help_text(saved_help_text.into());
            log::info!("hotkey capture commit succeeded");
        }
        Err(err) => {
            reset_hotkey_capture_state();
            log::warn!("hotkey capture commit failed: {}", err);
            app.set_status_text(format!("快捷键设置失败：{}", err).into());
            app.set_hotkey_help_text(
                format!("保存失败：{}。请换一个按键重试；Esc 取消", err).into(),
            );
        }
    }
}

fn take_pending_hotkey_capture_for_release(
    text: &str,
    modifiers: CapturedModifiers,
) -> Option<String> {
    let released_tokens = released_tokens_from_captured_key(text, modifiers);
    if !released_tokens.is_empty() {
        let shortcut = take_pending_hotkey_capture_matching(&released_tokens);
        log::info!(
            "hotkey capture release parsed: text={:?}, modifiers={:?}, released_tokens={:?}, committed_shortcut={:?}",
            text,
            modifiers,
            released_tokens,
            shortcut
        );
        return shortcut;
    }

    if text.is_empty() {
        // On macOS, Slint can emit key-released events with an empty text payload for modifier
        // keys. Pending captures only exist for modifier-only shortcuts, so the first release
        // event is enough to finalize the shortcut.
        let shortcut = take_pending_hotkey_capture();
        log::info!(
            "hotkey capture release fallback: empty text, modifiers={:?}, committed_shortcut={:?}",
            modifiers,
            shortcut
        );
        return shortcut;
    }

    log::info!(
        "hotkey capture release ignored: text={:?}, modifiers={:?}",
        text,
        modifiers
    );
    None
}

fn released_tokens_from_captured_key(text: &str, modifiers: CapturedModifiers) -> Vec<String> {
    let mut tokens = Vec::new();

    if let Some(token) = key_token_from_captured_key(text, modifiers) {
        tokens.push(token);
    }

    #[cfg(target_os = "macos")]
    {
        if is_slint_key(text, slint::platform::Key::Alt)
            || is_slint_key(text, slint::platform::Key::AltGr)
        {
            for token in ["LeftOption", "RightOption"] {
                if !tokens.iter().any(|item| item == token) {
                    tokens.push(token.to_string());
                }
            }
        } else if is_slint_key(text, slint::platform::Key::Shift)
            || is_slint_key(text, slint::platform::Key::ShiftR)
        {
            for token in ["LeftShift", "RightShift"] {
                if !tokens.iter().any(|item| item == token) {
                    tokens.push(token.to_string());
                }
            }
        } else if is_slint_key(text, slint::platform::Key::ControlR)
            || is_slint_key(text, slint::platform::Key::MetaR)
            || is_slint_key(text, slint::platform::Key::Control)
            || is_slint_key(text, slint::platform::Key::Meta)
        {
            for token in ["LeftCommand", "RightCommand", "LeftCtrl", "RightCtrl"] {
                if !tokens.iter().any(|item| item == token) {
                    tokens.push(token.to_string());
                }
            }
        }
    }

    tokens
}

fn shortcut_from_captured_key(text: &str, modifiers: CapturedModifiers) -> Option<String> {
    let key_token = key_token_from_captured_key(text, modifiers)?;
    let mut parts = modifier_tokens_from_state(modifiers);
    parts.retain(|part| !modifier_token_conflicts(part, &key_token));
    parts.push(key_token);
    Some(join_shortcut_parts(parts))
}

fn key_token_from_captured_key(text: &str, modifiers: CapturedModifiers) -> Option<String> {
    if text.is_empty() {
        return None;
    }

    #[cfg(target_os = "macos")]
    {
        if is_slint_key(text, slint::platform::Key::Alt) {
            return native_modifier_side_token("LeftOption", "RightOption")
                .or_else(|| Some("LeftOption".to_string()));
        }

        if is_slint_key(text, slint::platform::Key::AltGr) {
            return native_modifier_side_token("LeftOption", "RightOption")
                .or_else(|| Some("RightOption".to_string()));
        }

        if (is_slint_key(text, slint::platform::Key::ControlR)
            || is_slint_key(text, slint::platform::Key::MetaR))
            && modifiers.control
            && !modifiers.meta
        {
            return native_modifier_side_token("LeftCommand", "RightCommand")
                .or_else(|| Some("RightCommand".to_string()));
        }

        if (is_slint_key(text, slint::platform::Key::Control)
            || is_slint_key(text, slint::platform::Key::Meta))
            && modifiers.control
            && !modifiers.meta
        {
            return native_modifier_side_token("LeftCommand", "RightCommand")
                .or_else(|| Some("LeftCommand".to_string()));
        }

        if (is_slint_key(text, slint::platform::Key::MetaR)
            || is_slint_key(text, slint::platform::Key::ControlR))
            && modifiers.meta
            && !modifiers.control
        {
            return native_modifier_side_token("LeftCtrl", "RightCtrl")
                .or_else(|| Some("RightCtrl".to_string()));
        }

        if (is_slint_key(text, slint::platform::Key::Meta)
            || is_slint_key(text, slint::platform::Key::Control))
            && modifiers.meta
            && !modifiers.control
        {
            return native_modifier_side_token("LeftCtrl", "RightCtrl")
                .or_else(|| Some("LeftCtrl".to_string()));
        }

        if is_slint_key(text, slint::platform::Key::Shift) {
            return native_modifier_side_token("LeftShift", "RightShift")
                .or_else(|| Some("LeftShift".to_string()));
        }

        if is_slint_key(text, slint::platform::Key::ShiftR) {
            return native_modifier_side_token("LeftShift", "RightShift")
                .or_else(|| Some("RightShift".to_string()));
        }
    }

    #[cfg(not(target_os = "macos"))]
    if is_slint_key(text, slint::platform::Key::AltGr) {
        return Some("RightAlt".to_string());
    }

    if is_slint_key(text, slint::platform::Key::Control)
        || is_slint_key(text, slint::platform::Key::ControlR)
    {
        return Some("Ctrl".to_string());
    }
    if is_slint_key(text, slint::platform::Key::Alt) {
        return Some("Alt".to_string());
    }
    if is_slint_key(text, slint::platform::Key::Shift)
        || is_slint_key(text, slint::platform::Key::ShiftR)
    {
        return Some("Shift".to_string());
    }
    if is_slint_key(text, slint::platform::Key::Meta)
        || is_slint_key(text, slint::platform::Key::MetaR)
    {
        return Some("Meta".to_string());
    }
    if is_slint_key(text, slint::platform::Key::Space) {
        return Some("Space".to_string());
    }
    if is_slint_key(text, slint::platform::Key::Tab) {
        return Some("Tab".to_string());
    }
    if is_slint_key(text, slint::platform::Key::Return) {
        return Some("Enter".to_string());
    }
    if is_slint_key(text, slint::platform::Key::Backspace) {
        return Some("Backspace".to_string());
    }
    if is_slint_key(text, slint::platform::Key::Delete) {
        return Some("Delete".to_string());
    }
    if is_slint_key(text, slint::platform::Key::UpArrow) {
        return Some("Up".to_string());
    }
    if is_slint_key(text, slint::platform::Key::DownArrow) {
        return Some("Down".to_string());
    }
    if is_slint_key(text, slint::platform::Key::LeftArrow) {
        return Some("Left".to_string());
    }
    if is_slint_key(text, slint::platform::Key::RightArrow) {
        return Some("Right".to_string());
    }
    if is_slint_key(text, slint::platform::Key::Home) {
        return Some("Home".to_string());
    }
    if is_slint_key(text, slint::platform::Key::End) {
        return Some("End".to_string());
    }
    if is_slint_key(text, slint::platform::Key::PageUp) {
        return Some("PageUp".to_string());
    }
    if is_slint_key(text, slint::platform::Key::PageDown) {
        return Some("PageDown".to_string());
    }

    for (key, token) in [
        (slint::platform::Key::F1, "F1"),
        (slint::platform::Key::F2, "F2"),
        (slint::platform::Key::F3, "F3"),
        (slint::platform::Key::F4, "F4"),
        (slint::platform::Key::F5, "F5"),
        (slint::platform::Key::F6, "F6"),
        (slint::platform::Key::F7, "F7"),
        (slint::platform::Key::F8, "F8"),
        (slint::platform::Key::F9, "F9"),
        (slint::platform::Key::F10, "F10"),
        (slint::platform::Key::F11, "F11"),
        (slint::platform::Key::F12, "F12"),
        (slint::platform::Key::F13, "F13"),
    ] {
        if is_slint_key(text, key) {
            return Some(token.to_string());
        }
    }

    let mut chars = text.chars();
    let ch = chars.next()?;
    if chars.next().is_some() || ch.is_control() {
        return None;
    }

    Some(ch.to_ascii_uppercase().to_string())
}

#[cfg(target_os = "macos")]
fn native_modifier_side_token(left: &str, right: &str) -> Option<String> {
    let left_pressed = shanji_platform::hotkeys::is_native_modifier_pressed(left);
    let right_pressed = shanji_platform::hotkeys::is_native_modifier_pressed(right);

    match (left_pressed, right_pressed) {
        (true, false) => Some(left.to_string()),
        (false, true) => Some(right.to_string()),
        _ => None,
    }
}

fn modifier_tokens_from_state(modifiers: CapturedModifiers) -> Vec<String> {
    let mut parts = Vec::new();

    #[cfg(target_os = "macos")]
    {
        if modifiers.control {
            parts.push("Command".to_string());
        }
        if modifiers.meta {
            parts.push("Ctrl".to_string());
        }
        if modifiers.alt {
            parts.push("Option".to_string());
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        if modifiers.control {
            parts.push("Ctrl".to_string());
        }
        if modifiers.meta {
            parts.push("Meta".to_string());
        }
        if modifiers.alt {
            parts.push("Alt".to_string());
        }
    }
    if modifiers.shift {
        parts.push("Shift".to_string());
    }

    parts
}

fn join_shortcut_parts(parts: Vec<String>) -> String {
    let mut unique = Vec::new();
    for part in parts {
        if !unique.contains(&part) {
            unique.push(part);
        }
    }

    let mut modifiers = unique
        .iter()
        .filter(|part| shortcut_is_modifier_only(part))
        .cloned()
        .collect::<Vec<_>>();
    modifiers.sort_by_key(|part| shortcut_part_order(part));

    let key = unique
        .iter()
        .find(|part| !shortcut_is_modifier_only(part))
        .cloned();

    if let Some(key) = key {
        modifiers.push(key);
    }

    modifiers.join("+")
}

fn shortcut_is_modifier_only(shortcut: &str) -> bool {
    shortcut.split('+').all(is_modifier_token)
}

fn modifier_token_conflicts(existing: &str, key_token: &str) -> bool {
    matches!(
        (existing, key_token),
        ("Alt", "RightAlt")
            | ("Option", "LeftOption")
            | ("Option", "RightOption")
            | ("Option", "Option")
            | ("Command", "LeftCommand")
            | ("Command", "RightCommand")
            | ("Command", "Command")
            | ("Ctrl", "LeftCtrl")
            | ("Ctrl", "RightCtrl")
            | ("Ctrl", "Ctrl")
            | ("Shift", "LeftShift")
            | ("Shift", "RightShift")
            | ("Alt", "Alt")
            | ("Shift", "Shift")
            | ("Meta", "Meta")
    )
}

fn shortcut_part_order(part: &str) -> u8 {
    match part {
        "LeftCommand" | "RightCommand" | "Command" => 0,
        "LeftCtrl" | "RightCtrl" | "Ctrl" => 1,
        "LeftOption" | "RightOption" | "RightAlt" | "Alt" | "Option" => 2,
        "LeftShift" | "RightShift" | "Shift" => 3,
        "Fn" => 4,
        "Meta" => 5,
        _ => 10,
    }
}

fn is_modifier_token(token: &str) -> bool {
    matches!(
        token,
        "LeftCommand"
            | "RightCommand"
            | "Command"
            | "LeftCtrl"
            | "RightCtrl"
            | "Ctrl"
            | "LeftOption"
            | "RightOption"
            | "RightAlt"
            | "Alt"
            | "Option"
            | "LeftShift"
            | "RightShift"
            | "Shift"
            | "Fn"
            | "Meta"
    )
}

fn is_slint_key(text: &str, key: slint::platform::Key) -> bool {
    let key_text = slint::SharedString::from(key);
    text == key_text.as_str()
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

fn run() -> Result<(), Box<dyn std::error::Error>> {
    init_logging();
    shanji_core::ort_runtime::init_onnx_runtime()?;
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
    app::preload_models();
    refresh_settings_from_app(&app);

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
            cancel_hotkey_capture(&app);
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
    app.on_set_active_prompt_requested(move |id| {
        if let (Some(app), Some(overlay)) = (weak.upgrade(), weak_overlay.upgrade()) {
            apply_result(&app, &overlay, app::set_active_prompt(id.to_string()));
            apply_settings_snapshot(&app, app::refresh_settings_window());
        }
    });

    let weak = app.as_weak();
    app.on_open_llm_settings_requested(move || {
        if let Some(app) = weak.upgrade() {
            app.set_show_settings_page(true);
            app.set_active_section(SettingsSection::Llm);
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
    app.on_delete_record_requested(move |id| {
        let _ = app::delete_history_record(id);
        if let Some(app) = weak.upgrade() {
            refresh_history_only(&app);
        }
    });

    app.on_play_audio_requested(move |id| {
        let _ = app::play_history_audio(id);
    });

    let weak = app.as_weak();
    app.on_retranscribe_requested(move |id| {
        if !app::begin_history_retranscribing(id) {
            return;
        }

        if let Some(app) = weak.upgrade() {
            refresh_history_only(&app);
            app.set_status_text(format!("正在使用离线模型重新转写历史记录 {}...", id).into());
        }

        let weak_done = weak.clone();
        std::thread::spawn(move || {
            let result = app::retranscribe_history(id);
            app::finish_history_retranscribing(id);
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = weak_done.upgrade() {
                    refresh_history_only(&app);
                    match result {
                        Ok(()) => {
                            app.set_status_text(format!("历史记录 {} 已重新转写并回写", id).into())
                        }
                        Err(err) => app.set_status_text(
                            format!("历史记录 {} 重新转写失败: {}", id, err).into(),
                        ),
                    }
                }
            });
        });
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

    app.on_set_network_proxy_mode_requested(move |index| {
        let _ = app::set_network_proxy_mode(index);
    });

    app.on_set_network_proxy_type_requested(move |index| {
        let _ = app::set_network_proxy_type(index);
    });

    app.on_set_network_proxy_host_requested(move |host| {
        let _ = app::set_network_proxy_host(host.to_string());
    });

    app.on_set_network_proxy_port_requested(move |port| {
        let _ = app::set_network_proxy_port(port.to_string());
    });

    app.on_set_network_proxy_username_requested(move |username| {
        let _ = app::set_network_proxy_username(username.to_string());
    });

    app.on_set_network_proxy_password_requested(move |password| {
        let _ = app::set_network_proxy_password(password.to_string());
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
                })
                .ok();
            });
        }
    });

    let weak = app.as_weak();
    app.on_test_network_proxy_requested(move || {
        if let Some(app) = weak.upgrade() {
            app.set_network_proxy_test_status_text("".into());
            app.set_network_proxy_test_status_type(0);
            app.set_network_proxy_testing(true);
            let weak2 = weak.clone();
            std::thread::spawn(move || {
                let result = app::test_network_proxy();
                slint::invoke_from_event_loop(move || {
                    if let Some(app) = weak2.upgrade() {
                        app.set_network_proxy_testing(false);
                        match result {
                            Ok(message) => {
                                app.set_network_proxy_test_status_text(message.into());
                                app.set_network_proxy_test_status_type(1);
                            }
                            Err(err) => {
                                app.set_network_proxy_test_status_text(format!("✗ {}", err).into());
                                app.set_network_proxy_test_status_type(2);
                            }
                        }
                    }
                })
                .ok();
            });
        }
    });

    let weak = app.as_weak();
    app.on_begin_hotkey_recording_requested(move || {
        if let Some(app) = weak.upgrade() {
            begin_hotkey_capture(&app);
            app.set_status_text("正在录制快捷键，请按下目标按键".into());
        }
    });

    let weak = app.as_weak();
    app.on_cancel_hotkey_recording_requested(move || {
        if let Some(app) = weak.upgrade() {
            cancel_hotkey_capture(&app);
            app.set_status_text("已取消快捷键录制".into());
            app.set_hotkey_help_text(
                hotkey_capture_cancelled_help_text(app.get_hotkey_current_text().as_str()).into(),
            );
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    app.on_hotkey_capture_key_pressed(move |text, alt, control, shift, meta, repeat| {
        if let (Some(app), Some(overlay)) = (weak.upgrade(), weak_overlay.upgrade()) {
            handle_hotkey_capture_pressed(
                &app,
                &overlay,
                text.as_str(),
                CapturedModifiers {
                    alt,
                    control,
                    shift,
                    meta,
                },
                repeat,
            );
        }
    });

    let weak = app.as_weak();
    let weak_overlay = overlay.as_weak();
    app.on_hotkey_capture_key_released(move |text, alt, control, shift, meta| {
        if let (Some(app), Some(overlay)) = (weak.upgrade(), weak_overlay.upgrade()) {
            handle_hotkey_capture_released(
                &app,
                &overlay,
                text.as_str(),
                CapturedModifiers {
                    alt,
                    control,
                    shift,
                    meta,
                },
            );
        }
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

    #[cfg(target_os = "macos")]
    let native_hotkey_capture_timer = slint::Timer::default();
    #[cfg(target_os = "macos")]
    {
        let weak = app.as_weak();
        let weak_overlay = overlay.as_weak();
        native_hotkey_capture_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(16),
            move || {
                if let (Some(app), Some(overlay)) = (weak.upgrade(), weak_overlay.upgrade()) {
                    poll_macos_native_hotkey_capture(&app, &overlay);
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

    Ok(slint::run_event_loop_until_quit()?)
}

fn main() {
    if let Err(err) = run() {
        let message = err.to_string();
        log::error!("startup failed: {}", message);
        show_startup_error_dialog(&message);
        eprintln!("startup failed: {}", message);
        std::process::exit(1);
    }
}

fn handle_platform_hotkey_event(
    app: &AppWindow,
    overlay: &OverlayWindow,
    event: shanji_platform::hotkeys::PlatformHotkeyEvent,
) {
    use shanji_platform::hotkeys::{HotkeyAction, HotkeyEventState};

    if app.get_hotkey_recording() {
        log::info!(
            "ignoring platform hotkey while capture is active: action={:?}, state={:?}",
            event.action,
            event.state
        );
        return;
    }

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
            let result = crate::audio_transcriber::request_stop()
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
    app.set_llm_enabled(snapshot.llm_enabled);
    app.set_llm_active_prompt_id(snapshot.llm_active_prompt_id.into());
    app.set_llm_provider_summary(snapshot.llm_provider_summary.into());
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
            settings.set_hotkey_summary_text(snapshot.hotkey_summary_text.into());
            settings.set_hotkey_current_text(snapshot.hotkey_current_text.into());
            settings.set_hotkey_help_text(snapshot.hotkey_help_text.into());
            settings.set_config_path_text(snapshot.config_path_text.into());
            settings.set_llm_enabled(snapshot.llm_enabled);
            settings.set_llm_base_url(snapshot.llm_base_url.into());
            settings.set_llm_api_key_saved(snapshot.llm_api_key_saved);
            settings.set_llm_model_name(snapshot.llm_model_name.into());
            settings.set_llm_system_prompt(snapshot.llm_system_prompt.into());
            settings.set_llm_active_prompt_id(snapshot.llm_active_prompt_id.into());
            settings.set_llm_test_status_type(snapshot.llm_test_status);
            settings.set_llm_test_status_text(snapshot.llm_test_status_text.into());
            settings.set_network_proxy_mode_index(snapshot.network_proxy_mode_index);
            settings.set_network_proxy_type_index(snapshot.network_proxy_type_index);
            settings.set_network_proxy_host(snapshot.network_proxy_host.into());
            settings.set_network_proxy_port(snapshot.network_proxy_port.into());
            settings.set_network_proxy_username(snapshot.network_proxy_username.into());
            settings.set_network_proxy_password_saved(snapshot.network_proxy_password_saved);
            settings.set_network_proxy_test_status_type(snapshot.network_proxy_test_status);
            settings
                .set_network_proxy_test_status_text(snapshot.network_proxy_test_status_text.into());
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
                    is_retranscribing: app::is_history_retranscribing_record_id(c.record_id),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn key_text(key: slint::platform::Key) -> String {
        slint::SharedString::from(key).to_string()
    }

    #[cfg(target_os = "macos")]
    fn left_alt_modifier_token() -> &'static str {
        "LeftOption"
    }

    #[cfg(not(target_os = "macos"))]
    fn left_alt_modifier_token() -> &'static str {
        "Alt"
    }

    #[test]
    fn modifier_release_with_matching_token_commits_pending_shortcut() {
        reset_hotkey_capture_state();
        set_pending_hotkey_capture(left_alt_modifier_token().to_string());

        let shortcut = take_pending_hotkey_capture_for_release(
            &key_text(slint::platform::Key::Alt),
            CapturedModifiers::default(),
        );

        assert_eq!(shortcut.as_deref(), Some(left_alt_modifier_token()));
        assert!(take_pending_hotkey_capture().is_none());
    }

    #[test]
    fn empty_release_text_commits_pending_modifier_shortcut() {
        reset_hotkey_capture_state();
        set_pending_hotkey_capture("Shift".to_string());

        let shortcut = take_pending_hotkey_capture_for_release("", CapturedModifiers::default());

        assert_eq!(shortcut.as_deref(), Some("Shift"));
        assert!(take_pending_hotkey_capture().is_none());
    }

    #[test]
    fn empty_release_text_commits_pending_modifier_combo() {
        reset_hotkey_capture_state();
        set_pending_hotkey_capture("Command+Shift".to_string());

        let shortcut = take_pending_hotkey_capture_for_release(
            "",
            CapturedModifiers {
                control: true,
                ..CapturedModifiers::default()
            },
        );

        assert_eq!(shortcut.as_deref(), Some("Command+Shift"));
        assert!(take_pending_hotkey_capture().is_none());
    }

    #[test]
    fn unrelated_release_token_keeps_pending_shortcut() {
        reset_hotkey_capture_state();
        set_pending_hotkey_capture(left_alt_modifier_token().to_string());

        let shortcut = take_pending_hotkey_capture_for_release(
            &key_text(slint::platform::Key::Shift),
            CapturedModifiers::default(),
        );

        assert!(shortcut.is_none());
        assert_eq!(
            take_pending_hotkey_capture().as_deref(),
            Some(left_alt_modifier_token())
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_alt_release_matches_pending_right_option_even_when_event_is_generic() {
        reset_hotkey_capture_state();
        set_pending_hotkey_capture("RightOption".to_string());

        let shortcut = take_pending_hotkey_capture_for_release(
            &key_text(slint::platform::Key::Alt),
            CapturedModifiers::default(),
        );

        assert_eq!(shortcut.as_deref(), Some("RightOption"));
        assert!(take_pending_hotkey_capture().is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_option_keys_are_captured_with_side_specific_tokens() {
        let modifiers = CapturedModifiers {
            alt: true,
            ..CapturedModifiers::default()
        };

        assert_eq!(
            key_token_from_captured_key(&key_text(slint::platform::Key::Alt), modifiers).as_deref(),
            Some("LeftOption")
        );
        assert_eq!(
            key_token_from_captured_key(&key_text(slint::platform::Key::AltGr), modifiers)
                .as_deref(),
            Some("RightOption")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_command_and_control_keys_are_captured_with_side_specific_tokens() {
        let command_modifiers = CapturedModifiers {
            control: true,
            ..CapturedModifiers::default()
        };
        let control_modifiers = CapturedModifiers {
            meta: true,
            ..CapturedModifiers::default()
        };

        assert_eq!(
            key_token_from_captured_key(
                &key_text(slint::platform::Key::Control),
                command_modifiers
            )
            .as_deref(),
            Some("LeftCommand")
        );
        assert_eq!(
            key_token_from_captured_key(
                &key_text(slint::platform::Key::ControlR),
                command_modifiers
            )
            .as_deref(),
            Some("RightCommand")
        );
        assert_eq!(
            key_token_from_captured_key(&key_text(slint::platform::Key::Meta), control_modifiers)
                .as_deref(),
            Some("LeftCtrl")
        );
        assert_eq!(
            key_token_from_captured_key(&key_text(slint::platform::Key::MetaR), control_modifiers)
                .as_deref(),
            Some("RightCtrl")
        );
    }
}

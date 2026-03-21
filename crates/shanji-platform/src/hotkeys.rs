use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use serde::{Deserialize, Serialize};
use shanji_core::config::HotkeyConfig;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock, RwLock};

#[cfg(target_os = "macos")]
#[path = "hotkeys_macos.rs"]
mod hotkeys_macos;

#[cfg(not(target_os = "macos"))]
#[path = "hotkeys_native_non_macos.rs"]
mod hotkeys_native_non_macos;

const NATIVE_MODIFIER_GUARD_DELAY_MS: u32 = 150;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HotkeyAction {
    ToggleRecording,
    PushToTalk,
    ToggleRewrite,
    OpenHistory,
    OpenMainWindow,
}

impl HotkeyAction {
    pub fn label(self) -> &'static str {
        match self {
            HotkeyAction::ToggleRecording => "Toggle recording",
            HotkeyAction::PushToTalk => "Push-to-talk",
            HotkeyAction::ToggleRewrite => "Toggle rewrite",
            HotkeyAction::OpenHistory => "Open history",
            HotkeyAction::OpenMainWindow => "Open main window",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HotkeyTrigger {
    Press,
    PressAndRelease,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyBinding {
    pub action: HotkeyAction,
    pub accelerator: String,
    pub normalized: String,
    pub trigger: HotkeyTrigger,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeySummary {
    pub bindings: Vec<HotkeyBinding>,
    pub conflicts: Vec<String>,
    pub unsupported: Vec<String>,
}

impl HotkeySummary {
    pub fn status_line(&self) -> String {
        let total = self.bindings.len();
        let press_and_release = self
            .bindings
            .iter()
            .filter(|binding| binding.trigger == HotkeyTrigger::PressAndRelease)
            .count();

        let issues = self.conflicts.len() + self.unsupported.len();

        if issues == 0 {
            format!(
                "Hotkeys: {} registered shape(s) / {} press-release action(s)",
                total, press_and_release
            )
        } else {
            format!(
                "Hotkeys: {} binding issue(s) / {} active shape(s)",
                issues, total
            )
        }
    }

    pub fn inventory_text(&self) -> String {
        let mut lines = self
            .bindings
            .iter()
            .map(|binding| {
                format!(
                    "{} -> {} ({})",
                    binding.action.label(),
                    binding.accelerator,
                    match binding.trigger {
                        HotkeyTrigger::Press => "press",
                        HotkeyTrigger::PressAndRelease => "press/release",
                    }
                )
            })
            .collect::<Vec<_>>();

        if !self.conflicts.is_empty() {
            lines.extend(
                self.conflicts
                    .iter()
                    .map(|conflict| format!("Conflict: {}", conflict)),
            );
        }

        if !self.unsupported.is_empty() {
            lines.extend(
                self.unsupported
                    .iter()
                    .map(|item| format!("Unsupported: {}", item)),
            );
        }

        if lines.is_empty() {
            "No hotkeys configured".to_string()
        } else {
            lines.join("\n")
        }
    }
}

pub fn summarize_hotkeys(config: &HotkeyConfig) -> HotkeySummary {
    let bindings = bindings_from_config(config);
    let conflicts = collect_conflicts(&bindings);
    let unsupported = collect_unsupported(&bindings);

    HotkeySummary {
        bindings,
        conflicts,
        unsupported,
    }
}

pub fn effective_push_to_talk_hold_delay_ms(config: &HotkeyConfig) -> u32 {
    let Some(normalized) = normalize_shortcut(&config.push_to_talk) else {
        return config.push_to_talk_hold_delay_ms;
    };

    if is_native_only_binding(&normalized) {
        config
            .push_to_talk_hold_delay_ms
            .max(NATIVE_MODIFIER_GUARD_DELAY_MS)
    } else {
        config.push_to_talk_hold_delay_ms
    }
}

pub fn canonicalize_shortcut(shortcut: &str) -> Option<String> {
    let cleaned = shortcut.trim();
    if cleaned.is_empty() {
        return None;
    }

    let mut parts = cleaned
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(normalize_token)
        .collect::<Vec<_>>();

    if parts.is_empty() {
        return None;
    }

    let key = parts.pop().unwrap_or_default();
    parts.sort_by_key(|token| modifier_order(token));
    parts.push(storage_token(&key).to_string());

    Some(parts.join("+"))
}

pub fn validate_shortcut(shortcut: &str) -> Result<String, String> {
    let normalized = normalize_shortcut(shortcut).ok_or_else(|| "快捷键不能为空".to_string())?;
    let canonical = canonicalize_shortcut(shortcut).unwrap_or_else(|| shortcut.trim().to_string());
    log::info!(
        "validate_shortcut: raw={}, normalized={}, canonical={}",
        shortcut,
        normalized,
        canonical
    );

    if is_modifier_only(&normalized) {
        if supports_modifier_only(&normalized) {
            log::info!(
                "validate_shortcut: accepted modifier-only shortcut={}",
                canonical
            );
            return Ok(canonical);
        }
        log::warn!(
            "validate_shortcut: modifier-only shortcut is unsupported on this platform: {}",
            canonical
        );
        return Err(format!("当前平台不支持单独使用 {}", canonical));
    }

    if parse_global_hotkey(&canonical).is_some() {
        log::info!("validate_shortcut: accepted parsed shortcut={}", canonical);
        Ok(canonical)
    } else {
        log::warn!(
            "validate_shortcut: parse_global_hotkey rejected shortcut={}",
            canonical
        );
        Err(format!("当前平台不支持 {}", canonical))
    }
}

pub fn bindings_from_config(config: &HotkeyConfig) -> Vec<HotkeyBinding> {
    #[cfg(target_os = "macos")]
    let definitions = [(
        HotkeyAction::PushToTalk,
        config.push_to_talk.as_str(),
        HotkeyTrigger::PressAndRelease,
    )];

    #[cfg(not(target_os = "macos"))]
    let definitions = [
        (
            HotkeyAction::ToggleRecording,
            config.toggle_recording.as_str(),
            HotkeyTrigger::Press,
        ),
        (
            HotkeyAction::PushToTalk,
            config.push_to_talk.as_str(),
            HotkeyTrigger::PressAndRelease,
        ),
        (
            HotkeyAction::ToggleRewrite,
            config.toggle_rewrite.as_str(),
            HotkeyTrigger::Press,
        ),
        (
            HotkeyAction::OpenHistory,
            config.open_history.as_str(),
            HotkeyTrigger::Press,
        ),
        (
            HotkeyAction::OpenMainWindow,
            config.open_main.as_str(),
            HotkeyTrigger::Press,
        ),
    ];

    definitions
        .into_iter()
        .filter_map(|(action, accelerator, trigger)| {
            let normalized = normalize_shortcut(accelerator)?;
            Some(HotkeyBinding {
                action,
                accelerator: accelerator.trim().to_string(),
                normalized,
                trigger,
            })
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEventState {
    Pressed,
    Released,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformHotkeyEvent {
    pub action: HotkeyAction,
    pub state: HotkeyEventState,
}

#[derive(Debug, Clone, Copy)]
struct RegisteredHotkey {
    action: HotkeyAction,
    trigger: HotkeyTrigger,
}

pub struct HotkeyRuntime {
    _manager: Option<GlobalHotKeyManager>,
    bindings: HashMap<u32, RegisteredHotkey>,
    #[cfg(target_os = "macos")]
    _native: Option<hotkeys_macos::NativeHotkeyRuntime>,
    #[cfg(not(target_os = "macos"))]
    _native: Option<hotkeys_native_non_macos::NativeHotkeyRuntime>,
}

static ACTIVE_BINDINGS: OnceLock<RwLock<HashMap<u32, RegisteredHotkey>>> = OnceLock::new();
static NATIVE_EVENTS: OnceLock<Mutex<VecDeque<PlatformHotkeyEvent>>> = OnceLock::new();

fn active_bindings() -> &'static RwLock<HashMap<u32, RegisteredHotkey>> {
    ACTIVE_BINDINGS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn native_events() -> &'static Mutex<VecDeque<PlatformHotkeyEvent>> {
    NATIVE_EVENTS.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn replace_active_bindings(bindings: HashMap<u32, RegisteredHotkey>) {
    *active_bindings().write().unwrap() = bindings;
}

pub(crate) fn push_native_event(event: PlatformHotkeyEvent) {
    native_events().lock().unwrap().push_back(event);
}

pub fn drain_native_events() -> Vec<PlatformHotkeyEvent> {
    let mut queue = native_events().lock().unwrap();
    queue.drain(..).collect()
}

impl HotkeyRuntime {
    pub fn register(config: &HotkeyConfig) -> Result<Self, String> {
        let mut manager = None;
        let mut bindings = HashMap::new();
        #[cfg(target_os = "macos")]
        let mut native = None;
        #[cfg(not(target_os = "macos"))]
        let mut native = None;
        let effective_hold_delay_ms = effective_push_to_talk_hold_delay_ms(config);

        for binding in bindings_from_config(config) {
            #[cfg(target_os = "macos")]
            if is_native_only_binding(&binding.normalized) {
                native = Some(hotkeys_macos::NativeHotkeyRuntime::new(
                    binding.normalized.clone(),
                    binding.action,
                    binding.trigger,
                    effective_hold_delay_ms,
                )?);
                continue;
            }

            #[cfg(not(target_os = "macos"))]
            if is_native_only_binding(&binding.normalized) {
                native = Some(hotkeys_native_non_macos::NativeHotkeyRuntime::new(
                    binding.action,
                    binding.trigger,
                    effective_hold_delay_ms,
                )?);
                continue;
            }

            if is_modifier_only(&binding.normalized) {
                continue;
            }

            let Some(hotkey) = parse_global_hotkey(&binding.accelerator) else {
                continue;
            };

            if manager.is_none() {
                manager = Some(
                    GlobalHotKeyManager::new()
                        .map_err(|err| format!("Failed to create hotkey manager: {}", err))?,
                );
            }

            let id = hotkey.id();
            manager.as_ref().unwrap().register(hotkey).map_err(|err| {
                format!("Failed to register hotkey {}: {}", binding.accelerator, err)
            })?;
            bindings.insert(
                id,
                RegisteredHotkey {
                    action: binding.action,
                    trigger: binding.trigger,
                },
            );
        }

        replace_active_bindings(bindings.clone());

        Ok(Self {
            _manager: manager,
            bindings,
            #[cfg(target_os = "macos")]
            _native: native,
            #[cfg(not(target_os = "macos"))]
            _native: native,
        })
    }

    pub fn poll_events(&self) -> Vec<PlatformHotkeyEvent> {
        let receiver = GlobalHotKeyEvent::receiver();
        let mut actions = Vec::new();

        while let Ok(event) = receiver.try_recv() {
            let Some(binding) = self.bindings.get(&event.id) else {
                continue;
            };

            match (binding.trigger, event.state) {
                (HotkeyTrigger::Press, HotKeyState::Pressed) => actions.push(PlatformHotkeyEvent {
                    action: binding.action,
                    state: HotkeyEventState::Pressed,
                }),
                (HotkeyTrigger::PressAndRelease, HotKeyState::Pressed) => {
                    actions.push(PlatformHotkeyEvent {
                        action: binding.action,
                        state: HotkeyEventState::Pressed,
                    })
                }
                (HotkeyTrigger::PressAndRelease, HotKeyState::Released) => {
                    actions.push(PlatformHotkeyEvent {
                        action: binding.action,
                        state: HotkeyEventState::Released,
                    })
                }
                _ => {}
            }
        }

        actions
    }

    pub fn registered_count(&self) -> usize {
        self.bindings.len()
    }
}

impl Drop for HotkeyRuntime {
    fn drop(&mut self) {
        replace_active_bindings(HashMap::new());
    }
}

pub fn translate_global_event(event: GlobalHotKeyEvent) -> Option<PlatformHotkeyEvent> {
    let binding = active_bindings().read().unwrap().get(&event.id).copied()?;

    match (binding.trigger, event.state) {
        (HotkeyTrigger::Press, HotKeyState::Pressed) => Some(PlatformHotkeyEvent {
            action: binding.action,
            state: HotkeyEventState::Pressed,
        }),
        (HotkeyTrigger::PressAndRelease, HotKeyState::Pressed) => Some(PlatformHotkeyEvent {
            action: binding.action,
            state: HotkeyEventState::Pressed,
        }),
        (HotkeyTrigger::PressAndRelease, HotKeyState::Released) => Some(PlatformHotkeyEvent {
            action: binding.action,
            state: HotkeyEventState::Released,
        }),
        _ => None,
    }
}

fn collect_conflicts(bindings: &[HotkeyBinding]) -> Vec<String> {
    let mut grouped: HashMap<&str, Vec<&HotkeyBinding>> = HashMap::new();

    for binding in bindings {
        grouped
            .entry(binding.normalized.as_str())
            .or_default()
            .push(binding);
    }

    let mut conflicts = grouped
        .into_values()
        .filter(|group| group.len() > 1)
        .map(|group| {
            let actions = group
                .iter()
                .map(|binding| binding.action.label())
                .collect::<Vec<_>>()
                .join(", ");
            format!("{} share {}", actions, group[0].accelerator)
        })
        .collect::<Vec<_>>();

    conflicts.sort();
    conflicts
}

fn collect_unsupported(bindings: &[HotkeyBinding]) -> Vec<String> {
    bindings
        .iter()
        .filter(|binding| {
            if is_modifier_only(&binding.normalized) {
                !supports_modifier_only(&binding.normalized)
            } else {
                parse_global_hotkey(&binding.accelerator).is_none()
            }
        })
        .map(|binding| {
            if is_modifier_only(&binding.normalized) {
                format!(
                    "{} uses modifier-only shortcut {}",
                    binding.action.label(),
                    binding.accelerator
                )
            } else {
                format!(
                    "{} uses unsupported shortcut {}",
                    binding.action.label(),
                    binding.accelerator
                )
            }
        })
        .collect()
}

fn is_modifier_only(normalized: &str) -> bool {
    matches!(
        normalized,
        "Command"
            | "LeftCommand"
            | "Ctrl"
            | "LeftCtrl"
            | "Option"
            | "LeftOption"
            | "Shift"
            | "LeftShift"
            | "RightCommand"
            | "RightCtrl"
            | "RightOption"
            | "RightShift"
            | "RightAlt"
            | "Fn"
            | "Command+Shift"
            | "Command+Option"
            | "Ctrl+Shift"
    )
}

#[cfg(target_os = "macos")]
fn is_native_only_binding(normalized: &str) -> bool {
    matches!(
        normalized,
        "Command"
            | "LeftCommand"
            | "RightCommand"
            | "Option"
            | "LeftOption"
            | "RightOption"
            | "Ctrl"
            | "LeftCtrl"
            | "RightCtrl"
            | "Shift"
            | "LeftShift"
            | "RightShift"
            | "Fn"
    )
}

#[cfg(not(target_os = "macos"))]
fn is_native_only_binding(_normalized: &str) -> bool {
    _normalized == "RightAlt"
}

#[cfg(target_os = "macos")]
fn supports_modifier_only(normalized: &str) -> bool {
    is_native_only_binding(normalized)
}

#[cfg(not(target_os = "macos"))]
fn supports_modifier_only(normalized: &str) -> bool {
    normalized == "RightAlt"
}

fn normalize_shortcut(shortcut: &str) -> Option<String> {
    let cleaned = shortcut.trim();
    if cleaned.is_empty() {
        return None;
    }

    let mut normalized_parts = cleaned
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(normalize_token)
        .collect::<Vec<_>>();

    if normalized_parts.is_empty() {
        return None;
    }

    let key = normalized_parts.pop().unwrap_or_default();
    normalized_parts.sort();
    normalized_parts.push(key);

    Some(normalized_parts.join("+"))
}

fn normalize_token(token: &str) -> String {
    match token.to_ascii_lowercase().as_str() {
        "leftcommand" | "left-command" | "left_cmd" | "leftcmd" | "lcmd" => {
            "LeftCommand".to_string()
        }
        "rightcommand" | "right-command" | "right_cmd" | "rightcmd" | "rcmd" => {
            "RightCommand".to_string()
        }
        "leftctrl" | "left-control" | "leftcontrol" | "lctrl" | "lcontrol" => {
            "LeftCtrl".to_string()
        }
        "rightctrl" | "right-control" | "rightcontrol" | "rctrl" | "rcontrol" => {
            "RightCtrl".to_string()
        }
        "leftshift" | "left-shift" | "lshift" => "LeftShift".to_string(),
        "rightshift" | "right-shift" | "rshift" => "RightShift".to_string(),
        "leftoption" | "left-option" | "leftalt" | "left-alt" | "loption" | "lalt" => {
            "LeftOption".to_string()
        }
        "rightoption" | "right-option" | "roption" => "RightOption".to_string(),
        #[cfg(target_os = "macos")]
        "rightalt" | "right-alt" | "ralt" | "altgr" => "RightOption".to_string(),
        #[cfg(not(target_os = "macos"))]
        "rightalt" | "right-alt" | "ralt" | "altgr" => "RightAlt".to_string(),
        "cmd" | "command" | "meta" | "super" => "Command".to_string(),
        "ctrl" | "control" => "Ctrl".to_string(),
        "alt" | "option" => "Option".to_string(),
        "shift" => "Shift".to_string(),
        "fn" | "function" => "Fn".to_string(),
        "space" => "Space".to_string(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => {
                    let mut out = first.to_uppercase().collect::<String>();
                    out.push_str(chars.as_str());
                    out
                }
                None => String::new(),
            }
        }
    }
}

fn parse_global_hotkey(shortcut: &str) -> Option<HotKey> {
    let parts = shortcut
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();

    if parts.is_empty() {
        return None;
    }

    let mut modifiers = Modifiers::empty();
    let mut code = None;

    for part in &parts {
        match normalize_token(part).as_str() {
            "Command" => modifiers |= Modifiers::SUPER,
            "Ctrl" => modifiers |= Modifiers::CONTROL,
            "Option" => modifiers |= Modifiers::ALT,
            "Shift" => modifiers |= Modifiers::SHIFT,
            "LeftCommand" | "RightCommand" | "LeftCtrl" | "RightCtrl" | "LeftOption"
            | "RightOption" | "LeftShift" | "RightShift" | "Fn" | "RightAlt" => return None,
            token => {
                let parsed = parse_code(token)?;
                if code.replace(parsed).is_some() {
                    return None;
                }
            }
        }
    }

    code.map(|code| HotKey::new(Some(modifiers), code))
}

fn modifier_order(token: &str) -> u8 {
    match token {
        "Command" | "LeftCommand" | "RightCommand" => 0,
        "Ctrl" | "LeftCtrl" | "RightCtrl" => 1,
        "Option" | "LeftOption" | "RightOption" | "RightAlt" => 2,
        "Shift" | "LeftShift" | "RightShift" => 3,
        "Fn" => 4,
        _ => 10,
    }
}

#[cfg(target_os = "macos")]
fn storage_token(token: &str) -> &str {
    token
}

#[cfg(not(target_os = "macos"))]
fn storage_token(token: &str) -> &str {
    match token {
        "Option" => "Alt",
        other => other,
    }
}

#[cfg(target_os = "macos")]
pub fn is_native_modifier_pressed(normalized: &str) -> bool {
    hotkeys_macos::is_modifier_pressed(normalized)
}

#[cfg(not(target_os = "macos"))]
pub fn is_native_modifier_pressed(_normalized: &str) -> bool {
    false
}

fn parse_code(key: &str) -> Option<Code> {
    if key.len() == 1 {
        return match key.chars().next()?.to_ascii_uppercase() {
            'A' => Some(Code::KeyA),
            'B' => Some(Code::KeyB),
            'C' => Some(Code::KeyC),
            'D' => Some(Code::KeyD),
            'E' => Some(Code::KeyE),
            'F' => Some(Code::KeyF),
            'G' => Some(Code::KeyG),
            'H' => Some(Code::KeyH),
            'I' => Some(Code::KeyI),
            'J' => Some(Code::KeyJ),
            'K' => Some(Code::KeyK),
            'L' => Some(Code::KeyL),
            'M' => Some(Code::KeyM),
            'N' => Some(Code::KeyN),
            'O' => Some(Code::KeyO),
            'P' => Some(Code::KeyP),
            'Q' => Some(Code::KeyQ),
            'R' => Some(Code::KeyR),
            'S' => Some(Code::KeyS),
            'T' => Some(Code::KeyT),
            'U' => Some(Code::KeyU),
            'V' => Some(Code::KeyV),
            'W' => Some(Code::KeyW),
            'X' => Some(Code::KeyX),
            'Y' => Some(Code::KeyY),
            'Z' => Some(Code::KeyZ),
            '0' => Some(Code::Digit0),
            '1' => Some(Code::Digit1),
            '2' => Some(Code::Digit2),
            '3' => Some(Code::Digit3),
            '4' => Some(Code::Digit4),
            '5' => Some(Code::Digit5),
            '6' => Some(Code::Digit6),
            '7' => Some(Code::Digit7),
            '8' => Some(Code::Digit8),
            '9' => Some(Code::Digit9),
            _ => None,
        };
    }

    match key.to_ascii_lowercase().as_str() {
        "space" => Some(Code::Space),
        "enter" | "return" => Some(Code::Enter),
        "escape" | "esc" => Some(Code::Escape),
        "tab" => Some(Code::Tab),
        "backspace" => Some(Code::Backspace),
        "delete" | "del" => Some(Code::Delete),
        "up" => Some(Code::ArrowUp),
        "down" => Some(Code::ArrowDown),
        "left" => Some(Code::ArrowLeft),
        "right" => Some(Code::ArrowRight),
        "home" => Some(Code::Home),
        "end" => Some(Code::End),
        "pageup" | "page_up" => Some(Code::PageUp),
        "pagedown" | "page_down" => Some(Code::PageDown),
        "f1" => Some(Code::F1),
        "f2" => Some(Code::F2),
        "f3" => Some(Code::F3),
        "f4" => Some(Code::F4),
        "f5" => Some(Code::F5),
        "f6" => Some(Code::F6),
        "f7" => Some(Code::F7),
        "f8" => Some(Code::F8),
        "f9" => Some(Code::F9),
        "f10" => Some(Code::F10),
        "f11" => Some(Code::F11),
        "f12" => Some(Code::F12),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_bindings_and_conflicts() {
        let config = HotkeyConfig {
            toggle_recording: "Cmd+Shift+R".to_string(),
            push_to_talk: "Command+Shift+R".to_string(),
            push_to_talk_hold_delay_ms: 500,
            toggle_rewrite: "Ctrl+Shift+R".to_string(),
            open_history: "Ctrl+Shift+H".to_string(),
            open_main: "Ctrl+Shift+S".to_string(),
        };

        let summary = summarize_hotkeys(&config);

        #[cfg(target_os = "macos")]
        {
            assert_eq!(summary.bindings.len(), 1);
            assert!(summary.conflicts.is_empty());
            assert!(summary.unsupported.is_empty());
            assert!(summary.inventory_text().contains("Push-to-talk"));
        }

        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(summary.bindings.len(), 5);
            assert_eq!(summary.conflicts.len(), 1);
            assert!(summary.unsupported.is_empty());
            assert!(summary.inventory_text().contains("Push-to-talk"));
        }
    }

    #[test]
    fn reports_modifier_only_shortcuts_as_unsupported() {
        let config = HotkeyConfig {
            toggle_recording: "Command".to_string(),
            push_to_talk: "Option".to_string(),
            push_to_talk_hold_delay_ms: 500,
            toggle_rewrite: "Ctrl+Shift+R".to_string(),
            open_history: "Ctrl+Shift+H".to_string(),
            open_main: "Ctrl+Shift+S".to_string(),
        };

        let summary = summarize_hotkeys(&config);

        #[cfg(target_os = "macos")]
        assert_eq!(summary.unsupported.len(), 0);

        #[cfg(not(target_os = "macos"))]
        assert_eq!(summary.unsupported.len(), 2);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn accepts_right_command_as_supported_single_hotkey() {
        let config = HotkeyConfig {
            toggle_recording: String::new(),
            push_to_talk: "RightCommand".to_string(),
            push_to_talk_hold_delay_ms: 500,
            toggle_rewrite: String::new(),
            open_history: String::new(),
            open_main: String::new(),
        };

        let summary = summarize_hotkeys(&config);

        assert_eq!(summary.bindings.len(), 1);
        assert!(summary.unsupported.is_empty());
        assert_eq!(summary.bindings[0].normalized, "RightCommand");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn accepts_option_as_supported_single_hotkey() {
        let config = HotkeyConfig {
            toggle_recording: String::new(),
            push_to_talk: "Option".to_string(),
            push_to_talk_hold_delay_ms: 500,
            toggle_rewrite: String::new(),
            open_history: String::new(),
            open_main: String::new(),
        };

        let summary = summarize_hotkeys(&config);

        assert_eq!(summary.bindings.len(), 1);
        assert!(summary.unsupported.is_empty());
        assert_eq!(summary.bindings[0].normalized, "Option");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn accepts_side_specific_modifier_only_hotkeys() {
        for shortcut in ["LeftOption", "RightOption", "LeftCommand", "LeftCtrl", "Fn"] {
            let config = HotkeyConfig {
                toggle_recording: String::new(),
                push_to_talk: shortcut.to_string(),
                push_to_talk_hold_delay_ms: 500,
                toggle_rewrite: String::new(),
                open_history: String::new(),
                open_main: String::new(),
            };

            let summary = summarize_hotkeys(&config);

            assert_eq!(summary.bindings.len(), 1, "shortcut={shortcut}");
            assert!(summary.unsupported.is_empty(), "shortcut={shortcut}");
            assert_eq!(
                summary.bindings[0].normalized, shortcut,
                "shortcut={shortcut}"
            );
        }
    }
}

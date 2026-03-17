use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use serde::{Deserialize, Serialize};
use shanji_core::config::HotkeyConfig;
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

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

pub fn bindings_from_config(config: &HotkeyConfig) -> Vec<HotkeyBinding> {
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
    _manager: GlobalHotKeyManager,
    bindings: HashMap<u32, RegisteredHotkey>,
}

static ACTIVE_BINDINGS: OnceLock<RwLock<HashMap<u32, RegisteredHotkey>>> = OnceLock::new();

fn active_bindings() -> &'static RwLock<HashMap<u32, RegisteredHotkey>> {
    ACTIVE_BINDINGS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn replace_active_bindings(bindings: HashMap<u32, RegisteredHotkey>) {
    *active_bindings().write().unwrap() = bindings;
}

impl HotkeyRuntime {
    pub fn register(config: &HotkeyConfig) -> Result<Self, String> {
        let manager = GlobalHotKeyManager::new()
            .map_err(|err| format!("Failed to create hotkey manager: {}", err))?;
        let mut bindings = HashMap::new();

        for binding in bindings_from_config(config) {
            if is_modifier_only(&binding.normalized) {
                continue;
            }

            let Some(hotkey) = parse_global_hotkey(&binding.accelerator) else {
                continue;
            };

            let id = hotkey.id();
            manager.register(hotkey).map_err(|err| {
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
        .filter(|binding| is_modifier_only(&binding.normalized))
        .map(|binding| {
            format!(
                "{} uses modifier-only shortcut {}",
                binding.action.label(),
                binding.accelerator
            )
        })
        .collect()
}

fn is_modifier_only(normalized: &str) -> bool {
    matches!(
        normalized,
        "Command" | "Ctrl" | "Option" | "Shift" | "Command+Shift" | "Command+Option" | "Ctrl+Shift"
    )
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
        "cmd" | "command" | "meta" | "super" => "Command".to_string(),
        "ctrl" | "control" => "Ctrl".to_string(),
        "alt" | "option" => "Option".to_string(),
        "shift" => "Shift".to_string(),
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
        match part.to_ascii_lowercase().as_str() {
            "cmd" | "command" | "super" | "win" | "meta" => modifiers |= Modifiers::SUPER,
            "ctrl" | "control" => modifiers |= Modifiers::CONTROL,
            "alt" | "option" | "opt" => modifiers |= Modifiers::ALT,
            "shift" => modifiers |= Modifiers::SHIFT,
            token => code = parse_code(token),
        }
    }

    code.map(|code| HotKey::new(Some(modifiers), code))
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
            toggle_rewrite: "Ctrl+Shift+R".to_string(),
            open_history: "Ctrl+Shift+H".to_string(),
            open_main: "Ctrl+Shift+S".to_string(),
        };

        let summary = summarize_hotkeys(&config);

        assert_eq!(summary.bindings.len(), 5);
        assert_eq!(summary.conflicts.len(), 1);
        assert!(summary.unsupported.is_empty());
        assert!(summary.inventory_text().contains("Push-to-talk"));
    }

    #[test]
    fn reports_modifier_only_shortcuts_as_unsupported() {
        let config = HotkeyConfig {
            toggle_recording: "Command".to_string(),
            push_to_talk: "Option".to_string(),
            toggle_rewrite: "Ctrl+Shift+R".to_string(),
            open_history: "Ctrl+Shift+H".to_string(),
            open_main: "Ctrl+Shift+S".to_string(),
        };

        let summary = summarize_hotkeys(&config);

        assert_eq!(summary.unsupported.len(), 2);
    }
}

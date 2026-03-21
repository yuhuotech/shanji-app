use super::{HotkeyAction, HotkeyEventState, HotkeyTrigger, PlatformHotkeyEvent};
use readkey::Keycode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(10);

pub struct NativeHotkeyRuntime {
    stop_flag: Arc<AtomicBool>,
    join_handle: Option<thread::JoinHandle<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModifierBinding {
    LeftCommand,
    RightCommand,
    Command,
    LeftOption,
    RightOption,
    Option,
    LeftCtrl,
    RightCtrl,
    Ctrl,
    LeftShift,
    RightShift,
    Shift,
    Fn,
}

impl NativeHotkeyRuntime {
    pub fn new(
        normalized: String,
        action: HotkeyAction,
        trigger: HotkeyTrigger,
        hold_delay_ms: u32,
    ) -> Result<Self, String> {
        let binding = ModifierBinding::from_normalized(&normalized)
            .ok_or_else(|| format!("Unsupported macOS native-only shortcut: {}", normalized))?;
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop_flag);
        let hold_delay = Duration::from_millis(u64::from(hold_delay_ms));

        let join_handle = thread::Builder::new()
            .name(binding.thread_name().to_string())
            .spawn(move || {
                let mut tracker = ModifierHoldTracker::new(action, trigger, hold_delay);

                while !stop_thread.load(Ordering::Relaxed) {
                    let now = Instant::now();
                    let is_pressed = binding
                        .target_keys()
                        .iter()
                        .copied()
                        .any(Keycode::is_pressed);
                    let has_combo = other_combo_key_pressed(binding.target_keys());

                    for event in tracker.update(now, is_pressed, has_combo) {
                        super::push_native_event(event);
                    }

                    thread::sleep(POLL_INTERVAL);
                }
            })
            .map_err(|err| format!("Failed to spawn macOS {} watcher: {}", binding.label(), err))?;

        Ok(Self {
            stop_flag,
            join_handle: Some(join_handle),
        })
    }
}

impl Drop for NativeHotkeyRuntime {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

impl ModifierBinding {
    fn from_normalized(normalized: &str) -> Option<Self> {
        match normalized {
            "LeftCommand" => Some(Self::LeftCommand),
            "RightCommand" => Some(Self::RightCommand),
            "Command" => Some(Self::Command),
            "LeftOption" => Some(Self::LeftOption),
            "RightOption" => Some(Self::RightOption),
            "Option" => Some(Self::Option),
            "LeftCtrl" => Some(Self::LeftCtrl),
            "RightCtrl" => Some(Self::RightCtrl),
            "Ctrl" => Some(Self::Ctrl),
            "LeftShift" => Some(Self::LeftShift),
            "RightShift" => Some(Self::RightShift),
            "Shift" => Some(Self::Shift),
            "Fn" => Some(Self::Fn),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::LeftCommand => "LeftCommand",
            Self::RightCommand => "RightCommand",
            Self::Command => "Command",
            Self::LeftOption => "LeftOption",
            Self::RightOption => "RightOption",
            Self::Option => "Option",
            Self::LeftCtrl => "LeftCtrl",
            Self::RightCtrl => "RightCtrl",
            Self::Ctrl => "Ctrl",
            Self::LeftShift => "LeftShift",
            Self::RightShift => "RightShift",
            Self::Shift => "Shift",
            Self::Fn => "Fn",
        }
    }

    fn thread_name(self) -> &'static str {
        match self {
            Self::LeftCommand => "shanji-left-command",
            Self::RightCommand => "shanji-right-command",
            Self::Command => "shanji-command",
            Self::LeftOption => "shanji-left-option",
            Self::RightOption => "shanji-right-option",
            Self::Option => "shanji-option",
            Self::LeftCtrl => "shanji-left-control",
            Self::RightCtrl => "shanji-right-control",
            Self::Ctrl => "shanji-control",
            Self::LeftShift => "shanji-left-shift",
            Self::RightShift => "shanji-right-shift",
            Self::Shift => "shanji-shift",
            Self::Fn => "shanji-function",
        }
    }

    fn target_keys(self) -> &'static [Keycode] {
        match self {
            Self::LeftCommand => LEFT_COMMAND_KEYS,
            Self::RightCommand => RIGHT_COMMAND_KEYS,
            Self::Command => COMMAND_KEYS,
            Self::LeftOption => LEFT_OPTION_KEYS,
            Self::RightOption => RIGHT_OPTION_KEYS,
            Self::Option => OPTION_KEYS,
            Self::LeftCtrl => LEFT_CONTROL_KEYS,
            Self::RightCtrl => RIGHT_CONTROL_KEYS,
            Self::Ctrl => CONTROL_KEYS,
            Self::LeftShift => LEFT_SHIFT_KEYS,
            Self::RightShift => RIGHT_SHIFT_KEYS,
            Self::Shift => SHIFT_KEYS,
            Self::Fn => FN_KEYS,
        }
    }
}

pub fn is_modifier_pressed(normalized: &str) -> bool {
    ModifierBinding::from_normalized(normalized)
        .map(|binding| {
            binding
                .target_keys()
                .iter()
                .copied()
                .any(Keycode::is_pressed)
        })
        .unwrap_or(false)
}

#[derive(Debug)]
struct ModifierHoldTracker {
    action: HotkeyAction,
    trigger: HotkeyTrigger,
    hold_delay: Duration,
    pressed_at: Option<Instant>,
    combo_detected: bool,
    activated: bool,
}

impl ModifierHoldTracker {
    fn new(action: HotkeyAction, trigger: HotkeyTrigger, hold_delay: Duration) -> Self {
        Self {
            action,
            trigger,
            hold_delay,
            pressed_at: None,
            combo_detected: false,
            activated: false,
        }
    }

    fn update(
        &mut self,
        now: Instant,
        is_modifier_pressed: bool,
        has_combo_key: bool,
    ) -> Vec<PlatformHotkeyEvent> {
        let mut events = Vec::new();

        match (self.pressed_at, is_modifier_pressed) {
            (None, true) => {
                self.pressed_at = Some(now);
                self.combo_detected = has_combo_key;
                self.activated = false;

                if self.hold_delay.is_zero() && !self.combo_detected {
                    events.extend(self.activate());
                }
            }
            (Some(pressed_at), true) => {
                if !self.activated {
                    self.combo_detected |= has_combo_key;
                    if !self.combo_detected && now.duration_since(pressed_at) >= self.hold_delay {
                        events.extend(self.activate());
                    }
                }
            }
            (Some(_), false) => {
                if self.activated && matches!(self.trigger, HotkeyTrigger::PressAndRelease) {
                    events.push(PlatformHotkeyEvent {
                        action: self.action,
                        state: HotkeyEventState::Released,
                    });
                }
                self.reset();
            }
            (None, false) => {}
        }

        events
    }

    fn activate(&mut self) -> Vec<PlatformHotkeyEvent> {
        self.activated = true;

        if matches!(
            self.trigger,
            HotkeyTrigger::Press | HotkeyTrigger::PressAndRelease
        ) {
            vec![PlatformHotkeyEvent {
                action: self.action,
                state: HotkeyEventState::Pressed,
            }]
        } else {
            Vec::new()
        }
    }

    fn reset(&mut self) {
        self.pressed_at = None;
        self.combo_detected = false;
        self.activated = false;
    }
}

fn other_combo_key_pressed(target_keys: &[Keycode]) -> bool {
    OTHER_COMBO_KEYS
        .iter()
        .copied()
        .filter(|key| {
            !target_keys
                .iter()
                .any(|target| std::mem::discriminant(target) == std::mem::discriminant(key))
        })
        .any(Keycode::is_pressed)
}

const LEFT_COMMAND_KEYS: &[Keycode] = &[Keycode::Command];
const RIGHT_COMMAND_KEYS: &[Keycode] = &[Keycode::RightCommand];
const COMMAND_KEYS: &[Keycode] = &[Keycode::Command, Keycode::RightCommand];
const LEFT_OPTION_KEYS: &[Keycode] = &[Keycode::Option];
const RIGHT_OPTION_KEYS: &[Keycode] = &[Keycode::RightOption];
const OPTION_KEYS: &[Keycode] = &[Keycode::Option, Keycode::RightOption];
const LEFT_CONTROL_KEYS: &[Keycode] = &[Keycode::Control];
const RIGHT_CONTROL_KEYS: &[Keycode] = &[Keycode::RightControl];
const CONTROL_KEYS: &[Keycode] = &[Keycode::Control, Keycode::RightControl];
const LEFT_SHIFT_KEYS: &[Keycode] = &[Keycode::Shift];
const RIGHT_SHIFT_KEYS: &[Keycode] = &[Keycode::RightShift];
const SHIFT_KEYS: &[Keycode] = &[Keycode::Shift, Keycode::RightShift];
const FN_KEYS: &[Keycode] = &[Keycode::Function];

const OTHER_COMBO_KEYS: &[Keycode] = &[
    Keycode::Command,
    Keycode::Shift,
    Keycode::CapsLock,
    Keycode::Option,
    Keycode::Control,
    Keycode::RightShift,
    Keycode::RightOption,
    Keycode::RightControl,
    Keycode::Function,
    Keycode::Return,
    Keycode::Tab,
    Keycode::Space,
    Keycode::Delete,
    Keycode::Escape,
    Keycode::Help,
    Keycode::Home,
    Keycode::PageUp,
    Keycode::ForwardDelete,
    Keycode::End,
    Keycode::PageDown,
    Keycode::Left,
    Keycode::Right,
    Keycode::Down,
    Keycode::Up,
    Keycode::A,
    Keycode::S,
    Keycode::D,
    Keycode::F,
    Keycode::H,
    Keycode::G,
    Keycode::Z,
    Keycode::X,
    Keycode::C,
    Keycode::V,
    Keycode::B,
    Keycode::Q,
    Keycode::W,
    Keycode::E,
    Keycode::R,
    Keycode::Y,
    Keycode::T,
    Keycode::_1,
    Keycode::_2,
    Keycode::_3,
    Keycode::_4,
    Keycode::_5,
    Keycode::_6,
    Keycode::_7,
    Keycode::_8,
    Keycode::_9,
    Keycode::_0,
    Keycode::Equal,
    Keycode::Minus,
    Keycode::RightBracket,
    Keycode::O,
    Keycode::U,
    Keycode::LeftBracket,
    Keycode::I,
    Keycode::P,
    Keycode::L,
    Keycode::J,
    Keycode::Quote,
    Keycode::K,
    Keycode::Semicolon,
    Keycode::Backslash,
    Keycode::Comma,
    Keycode::Slash,
    Keycode::N,
    Keycode::M,
    Keycode::Period,
    Keycode::Grave,
    Keycode::KeypadDecimal,
    Keycode::KeypadMultiply,
    Keycode::KeypadPlus,
    Keycode::KeypadClear,
    Keycode::KeypadDivide,
    Keycode::KeypadEnter,
    Keycode::KeypadMinus,
    Keycode::KeypadEquals,
    Keycode::Keypad0,
    Keycode::Keypad1,
    Keycode::Keypad2,
    Keycode::Keypad3,
    Keycode::Keypad4,
    Keycode::Keypad5,
    Keycode::Keypad6,
    Keycode::Keypad7,
    Keycode::Keypad8,
    Keycode::Keypad9,
    Keycode::F1,
    Keycode::F2,
    Keycode::F3,
    Keycode::F4,
    Keycode::F5,
    Keycode::F6,
    Keycode::F7,
    Keycode::F8,
    Keycode::F9,
    Keycode::F10,
    Keycode::F11,
    Keycode::F12,
    Keycode::F13,
    Keycode::F14,
    Keycode::F15,
    Keycode::F16,
    Keycode::F17,
    Keycode::F18,
    Keycode::F19,
    Keycode::F20,
    Keycode::VolumeUp,
    Keycode::VolumeDown,
    Keycode::Mute,
    Keycode::Section,
    Keycode::Yen,
    Keycode::Underscore,
    Keycode::KeypadComma,
    Keycode::Eisu,
    Keycode::Kana,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activates_after_hold_delay_and_releases_after_key_up() {
        let mut tracker = ModifierHoldTracker::new(
            HotkeyAction::PushToTalk,
            HotkeyTrigger::PressAndRelease,
            Duration::from_millis(500),
        );
        let start = Instant::now();

        assert!(tracker.update(start, true, false).is_empty());
        assert!(tracker
            .update(start + Duration::from_millis(490), true, false)
            .is_empty());

        let pressed = tracker.update(start + Duration::from_millis(500), true, false);
        assert_eq!(
            pressed,
            vec![PlatformHotkeyEvent {
                action: HotkeyAction::PushToTalk,
                state: HotkeyEventState::Pressed,
            }]
        );

        let released = tracker.update(start + Duration::from_millis(520), false, false);
        assert_eq!(
            released,
            vec![PlatformHotkeyEvent {
                action: HotkeyAction::PushToTalk,
                state: HotkeyEventState::Released,
            }]
        );
    }

    #[test]
    fn cancels_activation_when_combo_key_appears_before_delay() {
        let mut tracker = ModifierHoldTracker::new(
            HotkeyAction::PushToTalk,
            HotkeyTrigger::PressAndRelease,
            Duration::from_millis(500),
        );
        let start = Instant::now();

        assert!(tracker.update(start, true, false).is_empty());
        assert!(tracker
            .update(start + Duration::from_millis(120), true, true)
            .is_empty());
        assert!(tracker
            .update(start + Duration::from_millis(700), true, false)
            .is_empty());
        assert!(tracker
            .update(start + Duration::from_millis(720), false, false)
            .is_empty());
    }

    #[test]
    fn ignores_quick_taps_shorter_than_hold_delay() {
        let mut tracker = ModifierHoldTracker::new(
            HotkeyAction::PushToTalk,
            HotkeyTrigger::PressAndRelease,
            Duration::from_millis(500),
        );
        let start = Instant::now();

        assert!(tracker.update(start, true, false).is_empty());
        assert!(tracker
            .update(start + Duration::from_millis(150), false, false)
            .is_empty());
    }
}

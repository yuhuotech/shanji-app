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

impl NativeHotkeyRuntime {
    pub fn new(
        action: HotkeyAction,
        trigger: HotkeyTrigger,
        hold_delay_ms: u32,
    ) -> Result<Self, String> {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop_flag);
        let hold_delay = Duration::from_millis(u64::from(hold_delay_ms));

        let join_handle = thread::Builder::new()
            .name("shanji-right-command".to_string())
            .spawn(move || {
                let mut tracker = RightCommandHoldTracker::new(action, trigger, hold_delay);

                while !stop_thread.load(Ordering::Relaxed) {
                    let now = Instant::now();
                    let is_pressed = Keycode::RightCommand.is_pressed();
                    let has_combo = other_combo_key_pressed();

                    for event in tracker.update(now, is_pressed, has_combo) {
                        super::push_native_event(event);
                    }

                    thread::sleep(POLL_INTERVAL);
                }
            })
            .map_err(|err| format!("Failed to spawn macOS RightCommand watcher: {}", err))?;

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

#[derive(Debug)]
struct RightCommandHoldTracker {
    action: HotkeyAction,
    trigger: HotkeyTrigger,
    hold_delay: Duration,
    pressed_at: Option<Instant>,
    combo_detected: bool,
    activated: bool,
}

impl RightCommandHoldTracker {
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
        is_right_command_pressed: bool,
        has_combo_key: bool,
    ) -> Vec<PlatformHotkeyEvent> {
        let mut events = Vec::new();

        match (self.pressed_at, is_right_command_pressed) {
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

fn other_combo_key_pressed() -> bool {
    OTHER_COMBO_KEYS.iter().copied().any(Keycode::is_pressed)
}

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
        let mut tracker = RightCommandHoldTracker::new(
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
        let mut tracker = RightCommandHoldTracker::new(
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
        let mut tracker = RightCommandHoldTracker::new(
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

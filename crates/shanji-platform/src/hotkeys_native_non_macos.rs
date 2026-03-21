use super::{HotkeyAction, HotkeyEventState, HotkeyTrigger, PlatformHotkeyEvent};
use device_query::{DeviceQuery, DeviceState, Keycode};
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
            .name("shanji-right-alt".to_string())
            .spawn(move || {
                let device_state = DeviceState::new();
                let mut tracker = RightAltHoldTracker::new(action, trigger, hold_delay);

                while !stop_thread.load(Ordering::Relaxed) {
                    let now = Instant::now();
                    let keys = device_state.get_keys();
                    let is_pressed = keys.contains(&Keycode::RAlt);
                    let has_combo = keys.iter().copied().any(is_combo_key);

                    for event in tracker.update(now, is_pressed, has_combo) {
                        super::push_native_event(event);
                    }

                    thread::sleep(POLL_INTERVAL);
                }
            })
            .map_err(|err| format!("Failed to spawn RightAlt watcher: {}", err))?;

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
struct RightAltHoldTracker {
    action: HotkeyAction,
    trigger: HotkeyTrigger,
    hold_delay: Duration,
    pressed_at: Option<Instant>,
    combo_detected: bool,
    activated: bool,
}

impl RightAltHoldTracker {
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
        is_right_alt_pressed: bool,
        has_combo_key: bool,
    ) -> Vec<PlatformHotkeyEvent> {
        let mut events = Vec::new();

        match (self.pressed_at, is_right_alt_pressed) {
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

fn is_combo_key(key: Keycode) -> bool {
    !matches!(key, Keycode::RAlt | Keycode::LControl | Keycode::RControl)
}

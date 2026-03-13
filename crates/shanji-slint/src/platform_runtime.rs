use shanji_core::config::{self, HotkeyConfig};
use shanji_core::paths;
use shanji_core::state;
use shanji_platform::hotkeys::{HotkeyRuntime, PlatformHotkeyEvent};
use shanji_platform::tray::{self, PlatformTrayEvent, TrayRuntime};

pub struct PlatformRuntime {
    hotkeys: Option<HotkeyRuntime>,
    tray: Option<TrayRuntime>,
    hotkey_signature: String,
    tray_signature: String,
    last_error: Option<String>,
}

impl PlatformRuntime {
    pub fn new() -> Self {
        Self {
            hotkeys: None,
            tray: None,
            hotkey_signature: String::new(),
            tray_signature: String::new(),
            last_error: None,
        }
    }

    pub fn sync(&mut self) -> Result<(), String> {
        let paths = paths::standard_app_paths("shanji").map_err(|err| err.to_string())?;
        config::init_config(&paths).map_err(|err| err.to_string())?;
        let config = config::get_config(&paths).map_err(|err| err.to_string())?;
        let hotkey_signature = hotkey_signature(&config.hotkeys);
        let runtime = state::get_runtime_snapshot();
        let tray_model =
            tray::build_tray_menu("Shanji", runtime.current_state.clone(), config.general.minimize_to_tray);
        let tray_signature = format!(
            "{}|{}|{}",
            tray_model.tooltip,
            tray_model.icon_state,
            tray_model.inventory_text()
        );

        if hotkey_signature != self.hotkey_signature {
            self.hotkeys = None;
            match HotkeyRuntime::register(&config.hotkeys) {
                Ok(runtime) => {
                    self.hotkeys = Some(runtime);
                    self.hotkey_signature = hotkey_signature;
                }
                Err(err) => {
                    self.hotkey_signature = hotkey_signature;
                    self.last_error = Some(err.clone());
                    return Err(err);
                }
            }
        }

        if self.tray.is_none() {
            let runtime = TrayRuntime::new(&tray_model)?;
            self.tray = Some(runtime);
            self.tray_signature = tray_signature;
        } else if tray_signature != self.tray_signature {
            if let Some(tray) = &self.tray {
                tray.update(&tray_model)?;
            }
            self.tray_signature = tray_signature;
        }

        self.last_error = None;
        Ok(())
    }

    pub fn poll_hotkey_events(&self) -> Vec<PlatformHotkeyEvent> {
        self.hotkeys
            .as_ref()
            .map(|runtime| runtime.poll_events())
            .unwrap_or_default()
    }

    pub fn poll_tray_events(&self) -> Vec<PlatformTrayEvent> {
        self.tray
            .as_ref()
            .map(|runtime| runtime.poll_events())
            .unwrap_or_default()
    }
}

fn hotkey_signature(config: &HotkeyConfig) -> String {
    [
        config.toggle_recording.as_str(),
        config.push_to_talk.as_str(),
        config.toggle_rewrite.as_str(),
        config.open_history.as_str(),
        config.open_main.as_str(),
    ]
    .join("|")
}

use serde::{Deserialize, Serialize};
use shanji_core::config::AppState;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const TRAY_ICON_ID: &str = "shanji-tray";
const OPEN_ITEM_ID: &str = "open-main";
const RECORD_ITEM_ID: &str = "toggle-recording";
const HISTORY_ITEM_ID: &str = "open-history";
const SETTINGS_ITEM_ID: &str = "open-settings";
const QUIT_ITEM_ID: &str = "quit-app";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrayAction {
    ShowMainWindow,
    ToggleRecording,
    OpenHistory,
    OpenSettings,
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrayMenuItemSpec {
    pub action: Option<TrayAction>,
    pub label: String,
    pub enabled: bool,
    pub separator: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrayMenuModel {
    pub tooltip: String,
    pub icon_state: String,
    pub items: Vec<TrayMenuItemSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformTrayEvent {
    Action(TrayAction),
    PrimaryClick,
}

pub struct TrayRuntime {
    tray_icon: TrayIcon,
    open_item: MenuItem,
    record_item: MenuItem,
    history_item: MenuItem,
    settings_item: MenuItem,
    quit_item: MenuItem,
}

impl TrayMenuModel {
    pub fn status_line(&self) -> String {
        format!(
            "Tray: {} / {} item(s)",
            self.icon_state,
            self.items.iter().filter(|item| !item.separator).count()
        )
    }

    pub fn inventory_text(&self) -> String {
        self.items
            .iter()
            .filter(|item| !item.separator)
            .map(|item| {
                format!(
                    "{}{}",
                    item.label,
                    if item.enabled { "" } else { " (disabled)" }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub fn build_tray_menu(app_name: &str, state: AppState, minimize_to_tray: bool) -> TrayMenuModel {
    let record_label = match state {
        AppState::Idle => "Start recording",
        AppState::Recording => "Stop recording",
        AppState::Transcribing => "Recording busy",
        AppState::Rewriting => "Rewrite in progress",
    };

    let icon_state = match state {
        AppState::Idle => "idle",
        AppState::Recording => "recording",
        AppState::Transcribing => "transcribing",
        AppState::Rewriting => "rewriting",
    }
    .to_string();

    let mut items = vec![
        TrayMenuItemSpec {
            action: Some(TrayAction::ShowMainWindow),
            label: "Open Shanji".to_string(),
            enabled: true,
            separator: false,
        },
        TrayMenuItemSpec {
            action: None,
            label: String::new(),
            enabled: false,
            separator: true,
        },
        TrayMenuItemSpec {
            action: Some(TrayAction::ToggleRecording),
            label: record_label.to_string(),
            enabled: !matches!(state, AppState::Transcribing | AppState::Rewriting),
            separator: false,
        },
        TrayMenuItemSpec {
            action: Some(TrayAction::OpenHistory),
            label: "Open history".to_string(),
            enabled: true,
            separator: false,
        },
        TrayMenuItemSpec {
            action: Some(TrayAction::OpenSettings),
            label: "Open settings".to_string(),
            enabled: true,
            separator: false,
        },
        TrayMenuItemSpec {
            action: None,
            label: String::new(),
            enabled: false,
            separator: true,
        },
        TrayMenuItemSpec {
            action: Some(TrayAction::Quit),
            label: "Quit".to_string(),
            enabled: true,
            separator: false,
        },
    ];

    if !minimize_to_tray {
        items.retain(|item| item.action != Some(TrayAction::ShowMainWindow) || !item.separator);
    }

    TrayMenuModel {
        tooltip: format!("{} native desktop app", app_name),
        icon_state,
        items,
    }
}

impl TrayRuntime {
    pub fn new(model: &TrayMenuModel) -> Result<Self, String> {
        let menu = Menu::new();
        let open_item = MenuItem::with_id(OPEN_ITEM_ID, "Open Shanji", true, None);
        let record_item = MenuItem::with_id(RECORD_ITEM_ID, "Start recording", true, None);
        let history_item = MenuItem::with_id(HISTORY_ITEM_ID, "Open history", true, None);
        let settings_item = MenuItem::with_id(SETTINGS_ITEM_ID, "Open settings", true, None);
        let quit_item = MenuItem::with_id(QUIT_ITEM_ID, "Quit", true, None);
        let separator_a = PredefinedMenuItem::separator();
        let separator_b = PredefinedMenuItem::separator();

        menu.append_items(&[
            &open_item,
            &separator_a,
            &record_item,
            &history_item,
            &settings_item,
            &separator_b,
            &quit_item,
        ])
        .map_err(|err| format!("Failed to build tray menu: {}", err))?;

        let tray_icon = TrayIconBuilder::new()
            .with_id(TRAY_ICON_ID)
            .with_icon(icon_for_state(&model.icon_state).map_err(|err| err.to_string())?)
            .with_tooltip(&model.tooltip)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            .map_err(|err| format!("Failed to create tray icon: {}", err))?;

        let runtime = Self {
            tray_icon,
            open_item,
            record_item,
            history_item,
            settings_item,
            quit_item,
        };
        runtime.update(model)?;
        Ok(runtime)
    }

    pub fn update(&self, model: &TrayMenuModel) -> Result<(), String> {
        self.tray_icon
            .set_icon(Some(
                icon_for_state(&model.icon_state).map_err(|err| err.to_string())?,
            ))
            .map_err(|err| format!("Failed to update tray icon: {}", err))?;
        self.tray_icon
            .set_tooltip(Some(&model.tooltip))
            .map_err(|err| format!("Failed to update tray tooltip: {}", err))?;

        for item in &model.items {
            match item.action {
                Some(TrayAction::ShowMainWindow) => {
                    self.open_item.set_text(&item.label);
                    self.open_item.set_enabled(item.enabled);
                }
                Some(TrayAction::ToggleRecording) => {
                    self.record_item.set_text(&item.label);
                    self.record_item.set_enabled(item.enabled);
                }
                Some(TrayAction::OpenHistory) => {
                    self.history_item.set_text(&item.label);
                    self.history_item.set_enabled(item.enabled);
                }
                Some(TrayAction::OpenSettings) => {
                    self.settings_item.set_text(&item.label);
                    self.settings_item.set_enabled(item.enabled);
                }
                Some(TrayAction::Quit) => {
                    self.quit_item.set_text(&item.label);
                    self.quit_item.set_enabled(item.enabled);
                }
                None => {}
            }
        }

        Ok(())
    }

    pub fn poll_events(&self) -> Vec<PlatformTrayEvent> {
        let mut events = Vec::new();

        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == *self.open_item.id() {
                events.push(PlatformTrayEvent::Action(TrayAction::ShowMainWindow));
            } else if event.id == *self.record_item.id() {
                events.push(PlatformTrayEvent::Action(TrayAction::ToggleRecording));
            } else if event.id == *self.history_item.id() {
                events.push(PlatformTrayEvent::Action(TrayAction::OpenHistory));
            } else if event.id == *self.settings_item.id() {
                events.push(PlatformTrayEvent::Action(TrayAction::OpenSettings));
            } else if event.id == *self.quit_item.id() {
                events.push(PlatformTrayEvent::Action(TrayAction::Quit));
            }
        }

        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            match event {
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
                | TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                } => events.push(PlatformTrayEvent::PrimaryClick),
                _ => {}
            }
        }

        events
    }
}

pub fn translate_menu_event(event: MenuEvent) -> Option<PlatformTrayEvent> {
    match event.id.0.as_str() {
        OPEN_ITEM_ID => Some(PlatformTrayEvent::Action(TrayAction::ShowMainWindow)),
        RECORD_ITEM_ID => Some(PlatformTrayEvent::Action(TrayAction::ToggleRecording)),
        HISTORY_ITEM_ID => Some(PlatformTrayEvent::Action(TrayAction::OpenHistory)),
        SETTINGS_ITEM_ID => Some(PlatformTrayEvent::Action(TrayAction::OpenSettings)),
        QUIT_ITEM_ID => Some(PlatformTrayEvent::Action(TrayAction::Quit)),
        _ => None,
    }
}

pub fn translate_tray_icon_event(event: TrayIconEvent) -> Option<PlatformTrayEvent> {
    if event.id().0 != TRAY_ICON_ID {
        return None;
    }

    match event {
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        }
        | TrayIconEvent::DoubleClick {
            button: MouseButton::Left,
            ..
        } => Some(PlatformTrayEvent::PrimaryClick),
        _ => None,
    }
}

fn icon_for_state(state: &str) -> Result<Icon, tray_icon::BadIcon> {
    let (r, g, b) = match state {
        "recording" => (0xD9, 0x5D, 0x39),
        "transcribing" => (0xE0, 0xA4, 0x3A),
        "rewriting" => (0x45, 0x79, 0xB5),
        _ => (0x18, 0x23, 0x2F),
    };

    let size = 16u32;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            if is_mic_pixel(x, y) {
                rgba.extend_from_slice(&[r, g, b, 255]);
            } else {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }

    Icon::from_rgba(rgba, size, size)
}

// 16×16 麦克风轮廓（根据 logo 主体简化）
//
//  ......XXXX......   row 1   顶部圆角
//  .....XXXXXX.....   row 2-7 话筒主体
//  ......XXXX......   row 8   底部圆角
//  ....X......X....   row 9-10 支架两侧
//  ....XXXXXXXX....   row 11  弧底
//  .......XX.......   row 12-13 竖杆
//  .....XXXXXX.....   row 14  底座
//
fn is_mic_pixel(x: u32, y: u32) -> bool {
    match y {
        1 => x >= 6 && x <= 9,
        2..=7 => x >= 5 && x <= 10,
        8 => x >= 6 && x <= 9,
        9..=10 => x == 4 || x == 11,
        11 => x >= 4 && x <= 11,
        12..=13 => x == 7 || x == 8,
        14 => x >= 5 && x <= 10,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_recording_tray_state() {
        let model = build_tray_menu("Shanji", AppState::Recording, true);

        assert_eq!(model.icon_state, "recording");
        assert!(model.inventory_text().contains("Stop recording"));
    }
}

use image::ImageReader;
use serde::{Deserialize, Serialize};
use shanji_core::config::AppState;
use std::io::Cursor;
use std::sync::OnceLock;
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const TRAY_ICON_ID: &str = "shanji-tray";
const OPEN_ITEM_ID: &str = "open-main";
const RECORD_ITEM_ID: &str = "toggle-recording";
const HISTORY_ITEM_ID: &str = "open-history";
const SETTINGS_ITEM_ID: &str = "open-settings";
const QUIT_ITEM_ID: &str = "quit-app";
const TRAY_ICON_BYTES: &[u8] =
    include_bytes!("../../shanji-slint/assets/tray/tray_icon_template_32.png");
static TRAY_ICON_RGBA: OnceLock<Result<TrayIconRgba, String>> = OnceLock::new();

#[derive(Debug, Clone)]
struct TrayIconRgba {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

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

pub fn build_tray_menu(app_name: &str, state: AppState, _minimize_to_tray: bool) -> TrayMenuModel {
    let icon_state = match state {
        AppState::Idle => "idle",
        AppState::Recording => "recording",
        AppState::Transcribing => "transcribing",
        AppState::Rewriting => "rewriting",
    }
    .to_string();

    let items = vec![
        TrayMenuItemSpec {
            action: Some(TrayAction::ShowMainWindow),
            label: "显示主界面".to_string(),
            enabled: true,
            separator: false,
        },
        TrayMenuItemSpec {
            action: Some(TrayAction::OpenSettings),
            label: "系统设置".to_string(),
            enabled: true,
            separator: false,
        },
        TrayMenuItemSpec {
            action: Some(TrayAction::OpenHistory),
            label: "历史记录".to_string(),
            enabled: true,
            separator: false,
        },
        TrayMenuItemSpec {
            action: Some(TrayAction::Quit),
            label: "退出".to_string(),
            enabled: true,
            separator: false,
        },
    ];

    TrayMenuModel {
        tooltip: format!("{} native desktop app", app_name),
        icon_state,
        items,
    }
}

impl TrayRuntime {
    pub fn new(model: &TrayMenuModel) -> Result<Self, String> {
        let menu = Menu::new();
        let open_item = MenuItem::with_id(OPEN_ITEM_ID, "显示主界面", true, None);
        let settings_item = MenuItem::with_id(SETTINGS_ITEM_ID, "系统设置", true, None);
        let history_item = MenuItem::with_id(HISTORY_ITEM_ID, "历史记录", true, None);
        let quit_item = MenuItem::with_id(QUIT_ITEM_ID, "退出", true, None);

        menu.append_items(&[&open_item, &settings_item, &history_item, &quit_item])
            .map_err(|err| format!("Failed to build tray menu: {}", err))?;

        let tray_icon = TrayIconBuilder::new()
            .with_id(TRAY_ICON_ID)
            .with_icon(icon_for_state(&model.icon_state).map_err(|err| err.to_string())?)
            .with_icon_as_template(true)
            .with_tooltip(&model.tooltip)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(true)
            .build()
            .map_err(|err| format!("Failed to create tray icon: {}", err))?;

        let runtime = Self {
            tray_icon,
            open_item,
            history_item,
            settings_item,
            quit_item,
        };
        runtime.update(model)?;
        Ok(runtime)
    }

    pub fn update(&self, model: &TrayMenuModel) -> Result<(), String> {
        let icon = icon_for_state(&model.icon_state).map_err(|err| err.to_string())?;
        #[cfg(target_os = "macos")]
        self.tray_icon
            .set_icon_with_as_template(Some(icon), true)
            .map_err(|err| format!("Failed to update tray icon: {}", err))?;
        #[cfg(not(target_os = "macos"))]
        self.tray_icon
            .set_icon(Some(icon))
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
                Some(TrayAction::ToggleRecording) | None => {}
            }
        }

        Ok(())
    }

    pub fn poll_events(&self) -> Vec<PlatformTrayEvent> {
        let mut events = Vec::new();

        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == *self.open_item.id() {
                events.push(PlatformTrayEvent::Action(TrayAction::ShowMainWindow));
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
        HISTORY_ITEM_ID => Some(PlatformTrayEvent::Action(TrayAction::OpenHistory)),
        SETTINGS_ITEM_ID => Some(PlatformTrayEvent::Action(TrayAction::OpenSettings)),
        QUIT_ITEM_ID => Some(PlatformTrayEvent::Action(TrayAction::Quit)),
        RECORD_ITEM_ID => Some(PlatformTrayEvent::Action(TrayAction::ToggleRecording)),
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
    let _ = state;
    TRAY_ICON_RGBA
        .get_or_init(load_tray_icon_rgba)
        .as_ref()
        .map(Clone::clone)
        .map_err(|_| tray_icon::BadIcon::ByteCountNotDivisibleBy4 {
            byte_count: TRAY_ICON_BYTES.len(),
        })
        .and_then(|icon| Icon::from_rgba(icon.rgba, icon.width, icon.height))
}

fn load_tray_icon_rgba() -> Result<TrayIconRgba, String> {
    let reader = ImageReader::new(Cursor::new(TRAY_ICON_BYTES))
        .with_guessed_format()
        .map_err(|err| format!("Failed to detect tray icon format: {}", err))?;
    let image = reader
        .decode()
        .map_err(|err| format!("Failed to decode tray icon PNG: {}", err))?
        .into_rgba8();
    let (width, height) = image.dimensions();
    Ok(TrayIconRgba {
        rgba: image.into_raw(),
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_recording_tray_state() {
        let model = build_tray_menu("Shanji", AppState::Recording, true);

        assert_eq!(model.icon_state, "recording");
        assert!(model.inventory_text().contains("显示主界面"));
    }

    #[test]
    fn loads_embedded_tray_icon() {
        let icon = load_tray_icon_rgba().expect("tray icon should decode");
        let _ = icon;
    }
}

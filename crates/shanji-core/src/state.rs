use crate::config::AppState;
use once_cell::sync::OnceCell;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSnapshot {
    pub current_state: AppState,
    pub active_model_id: Option<String>,
    pub overlay_visible: bool,
    pub status_message: String,
    pub live_transcript: String,
    pub rewrite_preview: String,
    pub final_output: String,
    pub last_transcript: String,
    pub audio_level: f32,
}

impl Default for RuntimeSnapshot {
    fn default() -> Self {
        Self {
            current_state: AppState::Idle,
            active_model_id: None,
            overlay_visible: true,
            status_message: "Native runtime is idle".to_string(),
            live_transcript: String::new(),
            rewrite_preview: String::new(),
            final_output: String::new(),
            last_transcript: String::new(),
            audio_level: 0.0,
        }
    }
}

pub struct AppStateManager {
    pub current_state: AppState,
    pub recording_handle: Option<tokio::task::AbortHandle>,
    pub capture_stop_sender: Option<Sender<()>>,
    pub active_model_id: Option<String>,
    pub mic_test_running: bool,
    pub download_cancellations: HashMap<String, tokio::sync::oneshot::Sender<()>>,
    pub download_cancel_flags: HashMap<String, Arc<AtomicBool>>,
    pub overlay_visible: bool,
    pub status_message: String,
    pub live_transcript: String,
    pub rewrite_preview: String,
    pub final_output: String,
    pub last_transcript: String,
    pub audio_level: f32,
}

impl Default for AppStateManager {
    fn default() -> Self {
        Self {
            current_state: AppState::Idle,
            recording_handle: None,
            capture_stop_sender: None,
            active_model_id: None,
            mic_test_running: false,
            download_cancellations: HashMap::new(),
            download_cancel_flags: HashMap::new(),
            overlay_visible: true,
            status_message: "Native runtime is idle".to_string(),
            live_transcript: String::new(),
            rewrite_preview: String::new(),
            final_output: String::new(),
            last_transcript: String::new(),
            audio_level: 0.0,
        }
    }
}

static APP_STATE: OnceCell<Arc<RwLock<AppStateManager>>> = OnceCell::new();
static RUNTIME_EVENT_LISTENERS: OnceCell<Mutex<Vec<Sender<RuntimeEventKind>>>> = OnceCell::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeEventKind {
    Ui,
    Platform,
}

fn runtime_event_listeners() -> &'static Mutex<Vec<Sender<RuntimeEventKind>>> {
    RUNTIME_EVENT_LISTENERS.get_or_init(|| Mutex::new(Vec::new()))
}

fn notify_runtime_event(kind: RuntimeEventKind) {
    let mut listeners = runtime_event_listeners().lock().unwrap();
    listeners.retain(|tx| tx.send(kind).is_ok());
}

pub fn subscribe_runtime_events() -> Receiver<RuntimeEventKind> {
    let (tx, rx) = mpsc::channel();
    runtime_event_listeners().lock().unwrap().push(tx);
    rx
}

pub fn init_state() {
    let _ = APP_STATE.get_or_init(|| Arc::new(RwLock::new(AppStateManager::default())));
}

pub fn get_state() -> &'static Arc<RwLock<AppStateManager>> {
    APP_STATE.get_or_init(|| Arc::new(RwLock::new(AppStateManager::default())))
}

pub fn set_state(new_state: AppState) {
    let mut state = get_state().write();
    state.current_state = new_state;
    drop(state);
    notify_runtime_event(RuntimeEventKind::Platform);
}

pub fn get_current_state() -> AppState {
    get_state().read().current_state.clone()
}

pub fn is_recording() -> bool {
    matches!(get_state().read().current_state, AppState::Recording)
}

pub fn set_recording_handle(handle: tokio::task::AbortHandle) {
    let mut state = get_state().write();
    state.recording_handle = Some(handle);
}

pub fn set_capture_stop_sender(sender: Sender<()>) {
    let mut state = get_state().write();
    state.capture_stop_sender = Some(sender);
}

pub fn clear_recording_handle() {
    let mut state = get_state().write();
    if let Some(sender) = state.capture_stop_sender.take() {
        let _ = sender.send(());
    }
    if let Some(handle) = state.recording_handle.take() {
        handle.abort();
    }
}

pub fn set_download_cancellation(model_id: &str, sender: tokio::sync::oneshot::Sender<()>) {
    let mut state = get_state().write();
    state
        .download_cancellations
        .insert(model_id.to_string(), sender);
}

pub fn set_download_cancel_flag(model_id: &str, flag: Arc<AtomicBool>) {
    let mut state = get_state().write();
    state
        .download_cancel_flags
        .insert(model_id.to_string(), flag);
}

pub fn cancel_download(model_id: &str) -> bool {
    let mut state = get_state().write();
    if let Some(sender) = state.download_cancellations.remove(model_id) {
        let _ = sender.send(());
        return true;
    }
    if let Some(flag) = state.download_cancel_flags.get(model_id) {
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
        return true;
    }
    false
}

pub fn set_active_model(model_id: String) {
    let mut state = get_state().write();
    state.active_model_id = Some(model_id);
}

pub fn get_active_model() -> Option<String> {
    get_state().read().active_model_id.clone()
}

pub fn set_mic_test_running(running: bool) {
    let mut state = get_state().write();
    state.mic_test_running = running;
    drop(state);
    notify_runtime_event(RuntimeEventKind::Ui);
}

pub fn is_mic_test_running() -> bool {
    get_state().read().mic_test_running
}

pub fn set_overlay_visible(visible: bool) {
    let mut state = get_state().write();
    state.overlay_visible = visible;
    drop(state);
    notify_runtime_event(RuntimeEventKind::Ui);
}

pub fn set_status_message(message: impl Into<String>) {
    let mut state = get_state().write();
    state.status_message = message.into();
    drop(state);
    notify_runtime_event(RuntimeEventKind::Ui);
}

pub fn set_live_transcript(text: impl Into<String>) {
    let mut state = get_state().write();
    state.live_transcript = text.into();
    drop(state);
    notify_runtime_event(RuntimeEventKind::Ui);
}

pub fn set_rewrite_preview(text: impl Into<String>) {
    let mut state = get_state().write();
    state.rewrite_preview = text.into();
    drop(state);
    notify_runtime_event(RuntimeEventKind::Ui);
}

pub fn set_final_output(text: impl Into<String>) {
    let mut state = get_state().write();
    state.final_output = text.into();
    drop(state);
    notify_runtime_event(RuntimeEventKind::Ui);
}

pub fn set_last_transcript(text: impl Into<String>) {
    let mut state = get_state().write();
    state.last_transcript = text.into();
    drop(state);
    notify_runtime_event(RuntimeEventKind::Ui);
}

pub fn set_audio_level(level: f32) {
    let mut state = get_state().write();
    state.audio_level = level;
    drop(state);
    notify_runtime_event(RuntimeEventKind::Ui);
}

pub fn clear_runtime_feedback() {
    let mut state = get_state().write();
    state.live_transcript.clear();
    state.rewrite_preview.clear();
    state.final_output.clear();
    state.audio_level = 0.0;
    drop(state);
    notify_runtime_event(RuntimeEventKind::Ui);
}

pub fn reset_runtime() {
    let mut state = get_state().write();
    state.current_state = AppState::Idle;
    state.overlay_visible = false;
    state.status_message = "Native runtime is idle".to_string();
    state.live_transcript.clear();
    state.rewrite_preview.clear();
    state.final_output.clear();
    state.last_transcript.clear();
    state.audio_level = 0.0;
    drop(state);
    notify_runtime_event(RuntimeEventKind::Platform);
}

pub fn get_runtime_snapshot() -> RuntimeSnapshot {
    let state = get_state().read();
    RuntimeSnapshot {
        current_state: state.current_state.clone(),
        active_model_id: state.active_model_id.clone(),
        overlay_visible: state.overlay_visible,
        status_message: state.status_message.clone(),
        live_transcript: state.live_transcript.clone(),
        rewrite_preview: state.rewrite_preview.clone(),
        final_output: state.final_output.clone(),
        last_transcript: state.last_transcript.clone(),
        audio_level: state.audio_level,
    }
}

use shanji_core::paths::AppPaths;
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};

const MAX_RETRIES: u32 = 10;
const RETRY_DELAY_SECS: u64 = 5;

#[derive(Clone, Default)]
struct DownloadState {
    downloading: bool,
    progress: f32,
    status_text: String,
    last_error: Option<String>,
}

static STATE: OnceLock<Mutex<HashMap<String, DownloadState>>> = OnceLock::new();
static DOWNLOAD_EVENT_LISTENERS: OnceLock<Mutex<Vec<Sender<()>>>> = OnceLock::new();

fn state() -> &'static Mutex<HashMap<String, DownloadState>> {
    STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn download_event_listeners() -> &'static Mutex<Vec<Sender<()>>> {
    DOWNLOAD_EVENT_LISTENERS.get_or_init(|| Mutex::new(Vec::new()))
}

fn notify_download_changed() {
    let mut listeners = download_event_listeners().lock().unwrap();
    listeners.retain(|tx| tx.send(()).is_ok());
}

pub fn subscribe_download_events() -> Receiver<()> {
    let (tx, rx) = mpsc::channel();
    download_event_listeners().lock().unwrap().push(tx);
    rx
}

fn get_state(model_id: &str) -> DownloadState {
    state()
        .lock()
        .unwrap()
        .get(model_id)
        .cloned()
        .unwrap_or_default()
}

fn update_state(model_id: &str, update: impl FnOnce(&mut DownloadState)) {
    let mut all = state().lock().unwrap();
    let entry = all.entry(model_id.to_string()).or_default();
    update(entry);
    drop(all);
    notify_download_changed();
}

pub fn is_downloading(model_id: &str) -> bool {
    get_state(model_id).downloading
}

pub fn any_downloading() -> bool {
    state().lock().unwrap().values().any(|s| s.downloading)
}

pub fn get_progress(model_id: &str) -> f32 {
    get_state(model_id).progress
}

pub fn get_status_text(model_id: &str) -> String {
    get_state(model_id).status_text
}

pub fn get_last_error(model_id: &str) -> Option<String> {
    get_state(model_id).last_error
}

pub fn start(paths: AppPaths, model_id: String) -> Result<(), String> {
    {
        let mut all = state().lock().unwrap();
        let entry = all.entry(model_id.clone()).or_default();
        if entry.downloading {
            return Err(format!("Download already in progress for {}", model_id));
        }
        entry.downloading = true;
        entry.progress = 0.0;
        entry.last_error = None;
        entry.status_text = "准备下载...".to_string();
    }
    notify_download_changed();

    std::thread::spawn(move || {
        let mut attempt = 0u32;

        loop {
            attempt += 1;

            let result = shanji_core::model::download_model_bundle_with_progress(
                &paths,
                &model_id,
                |downloaded, total| {
                    let progress = if total > 0 {
                        (downloaded as f32 / total as f32).min(1.0)
                    } else {
                        0.0
                    };
                    let pct = (progress * 100.0).round() as u32;
                    update_state(&model_id, |s| {
                        s.progress = progress;
                        s.status_text = format!(
                            "正在下载 {:.0} / {:.0} MB  ({}%)",
                            downloaded as f64 / 1_048_576.0,
                            total as f64 / 1_048_576.0,
                            pct,
                        );
                    });
                },
            );

            match result {
                Ok(()) => {
                    update_state(&model_id, |s| {
                        s.downloading = false;
                        s.progress = 1.0;
                        s.last_error = None;
                        s.status_text = "下载完成，正在加载...".to_string();
                    });
                    // Trigger model preloading after download completes
                    let preload_paths = paths.clone();
                    let downloaded_model = model_id.clone();
                    std::thread::spawn(move || {
                        let config = match shanji_core::config::get_config(&preload_paths) {
                            Ok(c) => c,
                            Err(_) => return,
                        };
                        if downloaded_model == config.asr.live_model_id {
                            crate::audio_transcriber::preload_live(preload_paths);
                        } else if downloaded_model == config.asr.refine_model_id
                            && config.asr.refine_enabled
                        {
                            crate::audio_transcriber::preload_refine(preload_paths);
                        }
                    });
                    break;
                }
                Err(e) if attempt < MAX_RETRIES => {
                    eprintln!(
                        "[model_downloader] Attempt {}/{} failed for {}: {}. Retrying in {}s...",
                        attempt, MAX_RETRIES, model_id, e, RETRY_DELAY_SECS
                    );
                    update_state(&model_id, |s| {
                        s.status_text = format!(
                            "连接中断，{}s 后重试（第 {}/{} 次）...",
                            RETRY_DELAY_SECS, attempt, MAX_RETRIES
                        );
                    });
                    std::thread::sleep(std::time::Duration::from_secs(RETRY_DELAY_SECS));
                    update_state(&model_id, |s| {
                        s.status_text = format!("正在重连...（第 {} 次重试）", attempt + 1);
                    });
                }
                Err(e) => {
                    eprintln!(
                        "[model_downloader] All {} attempts failed for {}: {}",
                        MAX_RETRIES, model_id, e
                    );
                    update_state(&model_id, |s| {
                        s.downloading = false;
                        s.progress = 0.0;
                        s.status_text.clear();
                        s.last_error = Some(e.to_string());
                    });
                    break;
                }
            }
        }
    });

    Ok(())
}

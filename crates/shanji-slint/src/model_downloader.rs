use shanji_core::paths::AppPaths;
use std::sync::{Mutex, OnceLock};

const MAX_RETRIES: u32 = 10;
const RETRY_DELAY_SECS: u64 = 5;

struct DownloadState {
    downloading: bool,
    progress: f32,
    status_text: String,
    last_error: Option<String>,
}

static STATE: OnceLock<Mutex<DownloadState>> = OnceLock::new();

fn state() -> &'static Mutex<DownloadState> {
    STATE.get_or_init(|| {
        Mutex::new(DownloadState {
            downloading: false,
            progress: 0.0,
            status_text: String::new(),
            last_error: None,
        })
    })
}

pub fn is_downloading() -> bool {
    state().lock().unwrap().downloading
}

pub fn get_progress() -> f32 {
    state().lock().unwrap().progress
}

pub fn get_status_text() -> String {
    state().lock().unwrap().status_text.clone()
}

pub fn get_last_error() -> Option<String> {
    state().lock().unwrap().last_error.clone()
}

pub fn start(paths: AppPaths, model_id: String) -> Result<(), String> {
    {
        let mut s = state().lock().unwrap();
        if s.downloading {
            return Err("Download already in progress".to_string());
        }
        s.downloading = true;
        s.progress = 0.0;
        s.last_error = None;
        s.status_text = "准备下载...".to_string();
    }

    std::thread::spawn(move || {
        let mut attempt = 0u32;

        loop {
            attempt += 1;

            // Progress callback — updates shared state so the 450ms UI timer picks it up
            let result = shanji_core::model::download_model_with_progress(
                &paths,
                &model_id,
                |downloaded, total| {
                    let progress = if total > 0 {
                        (downloaded as f32 / total as f32).min(1.0)
                    } else {
                        0.0
                    };
                    let pct = (progress * 100.0).round() as u32;
                    let mut s = state().lock().unwrap();
                    s.progress = progress;
                    s.status_text = format!(
                        "正在下载 {:.0} / {:.0} MB  ({}%)",
                        downloaded as f64 / 1_048_576.0,
                        total as f64 / 1_048_576.0,
                        pct,
                    );
                },
            );

            match result {
                Ok(()) => {
                    let mut s = state().lock().unwrap();
                    s.downloading = false;
                    s.progress = 1.0;
                    s.last_error = None;
                    s.status_text = "下载完成，正在加载...".to_string();
                    break;
                }
                Err(e) if attempt < MAX_RETRIES => {
                    eprintln!(
                        "[model_downloader] Attempt {}/{} failed: {}. Retrying in {}s...",
                        attempt, MAX_RETRIES, e, RETRY_DELAY_SECS
                    );
                    {
                        let mut s = state().lock().unwrap();
                        s.status_text = format!(
                            "连接中断，{}s 后重试（第 {}/{} 次）...",
                            RETRY_DELAY_SECS, attempt, MAX_RETRIES
                        );
                    }
                    std::thread::sleep(std::time::Duration::from_secs(RETRY_DELAY_SECS));
                    {
                        let mut s = state().lock().unwrap();
                        s.status_text = format!("正在重连...（第 {} 次重试）", attempt + 1);
                    }
                }
                Err(e) => {
                    eprintln!(
                        "[model_downloader] All {} attempts failed: {}",
                        MAX_RETRIES, e
                    );
                    let mut s = state().lock().unwrap();
                    s.downloading = false;
                    s.progress = 0.0;
                    s.status_text = String::new();
                    s.last_error = Some(e.to_string());
                    break;
                }
            }
        }
    });

    Ok(())
}

use shanji_core::asr::{AsrConfig, AsrEngine};
use shanji_core::audio::{self, AudioCapture};
use shanji_core::config::{self, AppConfig, AppState};
use shanji_core::history::{HistoryDb, HistoryRecord};
use shanji_core::llm;
use shanji_core::output;
use shanji_core::paths::AppPaths;
use shanji_core::state;
use std::sync::{mpsc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

struct LiveAsrSession {
    stop_tx: mpsc::Sender<()>,
    join_handle: JoinHandle<()>,
}

static LIVE_ASR: OnceLock<Mutex<Option<LiveAsrSession>>> = OnceLock::new();

fn live_asr_slot() -> &'static Mutex<Option<LiveAsrSession>> {
    LIVE_ASR.get_or_init(|| Mutex::new(None))
}

pub fn toggle(paths: AppPaths) -> Result<bool, String> {
    if is_running() {
        stop()?;
        return Ok(false);
    }

    start(paths)?;
    Ok(true)
}

pub fn start(paths: AppPaths) -> Result<(), String> {
    if is_running() {
        return Ok(());
    }

    let mut slot = live_asr_slot()
        .lock()
        .map_err(|_| "Live ASR lock poisoned".to_string())?;
    let config = config::get_config(&paths).map_err(|e| e.to_string())?;
    let model_dir = shanji_core::model::get_model_dir_with_paths(&paths, &config.asr.model_id);
    if !shanji_core::model::is_model_downloaded_with_paths(&paths, &config.asr.model_id) {
        return Err(format!("Model not downloaded: {}", config.asr.model_id));
    }

    let (sample_tx, sample_rx) = mpsc::channel::<Vec<f32>>();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let selected_device = config.audio.device_name.clone();

    let join_handle = std::thread::spawn(move || {
        if let Err(err) = run_live_asr(paths, config, model_dir, selected_device, sample_tx, sample_rx, stop_rx) {
            state::set_status_message(format!("Live ASR failed: {}", err));
            state::set_state(AppState::Idle);
            state::set_overlay_visible(false);
            state::set_audio_level(0.0);
        }
    });

    *slot = Some(LiveAsrSession { stop_tx, join_handle });
    Ok(())
}

pub fn stop() -> Result<bool, String> {
    let mut slot = live_asr_slot()
        .lock()
        .map_err(|_| "Live ASR lock poisoned".to_string())?;

    let Some(session) = slot.take() else {
        return Ok(false);
    };

    let _ = session.stop_tx.send(());
    let _ = session.join_handle.join();
    Ok(true)
}

pub fn is_running() -> bool {
    live_asr_slot()
        .lock()
        .map(|slot| slot.is_some())
        .unwrap_or(false)
}

fn run_live_asr(
    paths: AppPaths,
    config: AppConfig,
    model_dir: std::path::PathBuf,
    selected_device: Option<String>,
    sample_tx: mpsc::Sender<Vec<f32>>,
    sample_rx: mpsc::Receiver<Vec<f32>>,
    stop_rx: mpsc::Receiver<()>,
) -> Result<(), String> {
    let mut capture = AudioCapture::new();
    capture
        .start(selected_device.as_deref(), sample_tx)
        .map_err(|e| e.to_string())?;

    let mut engine = AsrEngine::new(AsrConfig {
        insert_punct: config.asr.insert_punct,
        punct_style: config.asr.punct_style.clone(),
        ..AsrConfig::default()
    })
    .map_err(|e| e.to_string())?;
    engine.load_model(&model_dir).map_err(|e| e.to_string())?;

    state::set_state(AppState::Recording);
    state::set_overlay_visible(true);
    state::set_status_message(format!("Live ASR started with {}", config.asr.model_id));
    state::clear_runtime_feedback();

    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }

        match sample_rx.recv_timeout(Duration::from_millis(120)) {
            Ok(samples) => {
                let level = audio::calculate_audio_level(&samples);
                state::set_audio_level(level);

                match engine.process_chunk(&samples) {
                    Ok(Some(text)) if !text.is_empty() => {
                        state::set_state(AppState::Transcribing);
                        state::set_status_message("Live ASR is producing partial text");
                        state::set_live_transcript(text.clone());
                        state::set_last_transcript(text);
                    }
                    Ok(_) => {}
                    Err(err) => {
                        state::set_status_message(format!("ASR chunk failed: {}", err));
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    capture.stop();

    let transcribed_text = engine.finalize().map_err(|e| e.to_string())?;
    if !transcribed_text.is_empty() {
        state::set_last_transcript(transcribed_text.clone());

        let rewritten_text = maybe_rewrite_text(&config, &transcribed_text);
        if let Some(ref rewritten) = rewritten_text {
            state::set_rewrite_preview(rewritten.clone());
        }

        let output_source = rewritten_text
            .clone()
            .unwrap_or_else(|| transcribed_text.clone());
        match output::deliver_output(&output_source, &config.output) {
            Ok(delivery) => {
                state::set_final_output(delivery.formatted_text.clone());
                if let Some(err) = delivery.auto_paste_error {
                    state::set_status_message(format!(
                        "Auto-paste unavailable, copied to clipboard instead: {}",
                        err
                    ));
                }
            }
            Err(err) => {
                let formatted_output = output::format_output(&output_source, &config.output);
                state::set_final_output(formatted_output);
                state::set_status_message(format!("Output delivery failed: {}", err));
            }
        }

        save_final_history(&paths, &config, &transcribed_text, rewritten_text.as_deref())?;
    }

    state::set_live_transcript("");
    state::set_audio_level(0.0);
    state::set_overlay_visible(false);
    state::set_state(AppState::Idle);
    state::set_status_message(if transcribed_text.is_empty() {
        "Live ASR stopped with no final transcript"
    } else {
        "Live ASR stopped, output delivered"
    });

    Ok(())
}

fn maybe_rewrite_text(config: &AppConfig, text: &str) -> Option<String> {
    if !config.rewrite.enabled || config.rewrite.active_provider_id.is_empty() {
        return None;
    }

    state::set_state(AppState::Rewriting);
    state::set_status_message("Running rewrite provider for final transcript");

    match llm::create_client(&config.rewrite.active_provider_id, &config.rewrite) {
        Ok(client) => match client.rewrite(text) {
            Ok(rewritten) if !rewritten.is_empty() => Some(rewritten),
            Ok(_) => None,
            Err(err) => {
                state::set_status_message(format!("Rewrite failed, keeping original text: {}", err));
                None
            }
        },
        Err(err) => {
            state::set_status_message(format!("Rewrite unavailable, keeping original text: {}", err));
            None
        }
    }
}

fn save_final_history(
    paths: &AppPaths,
    config: &AppConfig,
    transcribed: &str,
    rewritten: Option<&str>,
) -> Result<(), String> {
    let db = HistoryDb::new_with_paths(paths).map_err(|e| e.to_string())?;
    let record = HistoryRecord {
        id: None,
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        transcribed: transcribed.to_string(),
        rewritten: rewritten.map(|text| text.to_string()),
        duration_ms: None,
        model_id: Some(config.asr.model_id.clone()),
        provider_id: rewritten.map(|_| config.rewrite.active_provider_id.clone()),
    };
    db.insert(&record).map_err(|e| e.to_string())?;
    Ok(())
}

use shanji_core::audio::{self, AudioCapture};
use shanji_core::state;
use std::sync::{mpsc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

struct MicMonitor {
    stop_tx: mpsc::Sender<()>,
    join_handle: JoinHandle<()>,
}

static MIC_MONITOR: OnceLock<Mutex<Option<MicMonitor>>> = OnceLock::new();

fn monitor_slot() -> &'static Mutex<Option<MicMonitor>> {
    MIC_MONITOR.get_or_init(|| Mutex::new(None))
}

pub fn toggle(device_name: Option<String>) -> Result<bool, String> {
    if is_running() {
        stop()?;
        return Ok(false);
    }

    let mut slot = monitor_slot()
        .lock()
        .map_err(|_| "Microphone monitor lock poisoned".to_string())?;
    let (sample_tx, sample_rx) = mpsc::channel::<Vec<f32>>();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let selected_device = device_name.clone();

    let join_handle = std::thread::spawn(move || {
        let mut capture = AudioCapture::new();
        if let Err(err) = capture.start(selected_device.as_deref(), sample_tx) {
            state::set_status_message(format!("Microphone monitor failed: {}", err));
            state::set_mic_test_running(false);
            return;
        }

        state::set_mic_test_running(true);
        state::set_status_message("Monitoring microphone input from native app");

        loop {
            if stop_rx.try_recv().is_ok() {
                break;
            }

            match sample_rx.recv_timeout(Duration::from_millis(120)) {
                Ok(samples) => {
                    let level = audio::calculate_audio_level(&samples);
                    state::set_audio_level(level);
                    if matches!(
                        state::get_current_state(),
                        shanji_core::config::AppState::Idle
                    ) {
                        state::set_status_message(format!(
                            "Monitoring microphone input from native app ({}%)",
                            (level * 100.0).round() as u32
                        ));
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        capture.stop();
        state::set_mic_test_running(false);
        state::set_audio_level(0.0);
    });

    *slot = Some(MicMonitor {
        stop_tx,
        join_handle,
    });
    Ok(true)
}

pub fn stop() -> Result<bool, String> {
    let mut slot = monitor_slot()
        .lock()
        .map_err(|_| "Microphone monitor lock poisoned".to_string())?;

    let Some(monitor) = slot.take() else {
        return Ok(false);
    };

    let _ = monitor.stop_tx.send(());
    let _ = monitor.join_handle.join();
    state::set_mic_test_running(false);
    state::set_audio_level(0.0);
    state::set_status_message("Microphone monitor stopped");
    Ok(true)
}

pub fn is_running() -> bool {
    monitor_slot()
        .lock()
        .map(|slot| slot.is_some())
        .unwrap_or(false)
}

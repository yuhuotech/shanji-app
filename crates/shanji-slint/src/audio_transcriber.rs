use shanji_core::asr::{AsrConfig, AsrEngine};
use shanji_core::audio::{self, AudioCapture};
use shanji_core::config::{self, AppConfig, AppState};
use shanji_core::history::{HistoryDb, HistoryRecord};
use shanji_core::hotwords;
use shanji_core::llm;
use shanji_core::model;
use shanji_core::output;
use shanji_core::paths::AppPaths;
use shanji_core::state;
use shanji_core::text_processing::{
    finalize_transcript_text, render_segmented_transcript, TextProcessingConfig, TranscriptChunk,
};
use shanji_core::vad::{self, VadDetector, VadEvent};
use std::sync::{mpsc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct LiveAsrSession {
    stop_tx: mpsc::Sender<()>,
    join_handle: JoinHandle<()>,
}

struct PendingPaste {
    clipboard_backup: Option<String>,
}

#[derive(Clone)]
struct TranscriptSegment {
    id: u64,
    leading_pause_ms: u32,
    live_text: String,
    corrected_text: Option<String>,
}

struct RefineTask {
    segment_id: u64,
    audio: Vec<f32>,
    fallback_text: String,
}

struct RefineResult {
    segment_id: u64,
    text: String,
}

struct RefineWorker {
    task_tx: mpsc::Sender<RefineTask>,
    result_rx: mpsc::Receiver<RefineResult>,
    join_handle: JoinHandle<()>,
}

struct PendingSegment {
    leading_pause_ms: u32,
    audio: Vec<f32>,
    live_text: String,
}

const SEGMENT_OVERLAP_SAMPLES: usize = 3_200;
const MIN_REFINE_SEGMENT_SAMPLES: usize = 16_000;

static PENDING_PASTE: OnceLock<Mutex<Option<PendingPaste>>> = OnceLock::new();

fn pending_paste_slot() -> &'static Mutex<Option<PendingPaste>> {
    PENDING_PASTE.get_or_init(|| Mutex::new(None))
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
    config::init_config(&paths).map_err(|e| e.to_string())?;
    let config = config::get_config(&paths).map_err(|e| e.to_string())?;
    if !shanji_core::model::is_model_downloaded_with_paths(&paths, &config.asr.live_model_id) {
        return Err(format!(
            "Live model not downloaded: {}",
            config.asr.live_model_id
        ));
    }

    let (sample_tx, sample_rx) = mpsc::channel::<Vec<f32>>();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let selected_device = config.audio.device_name.clone();
    log::info!(
        "Starting live ASR session: live_model={}, refine_enabled={}, refine_model={}, device={:?}, noise_reduction={}, vad_threshold={}, vad_end_threshold={}, min_speech_frames={}, silence_timeout_ms={}, comma_pause_ms={}, sentence_pause_ms={}",
        config.asr.live_model_id,
        config.asr.refine_enabled,
        config.asr.refine_model_id,
        selected_device,
        config.audio.noise_reduction,
        config.audio.vad_threshold,
        config.audio.vad_end_threshold,
        config.audio.min_speech_frames,
        config.audio.silence_timeout_ms,
        config.asr.comma_pause_ms,
        config.asr.sentence_pause_ms
    );

    let join_handle = std::thread::spawn(move || {
        if let Err(err) = run_live_asr(
            paths,
            config,
            selected_device,
            sample_tx,
            sample_rx,
            stop_rx,
        ) {
            state::set_status_message(format!("Live ASR failed: {}", err));
            state::set_state(AppState::Idle);
            state::set_overlay_visible(false);
            state::set_audio_level(0.0);
        }
    });

    *slot = Some(LiveAsrSession {
        stop_tx,
        join_handle,
    });
    Ok(())
}

pub fn stop() -> Result<bool, String> {
    let mut slot = live_asr_slot()
        .lock()
        .map_err(|_| "Live ASR lock poisoned".to_string())?;

    let Some(session) = slot.take() else {
        return Ok(false);
    };

    log::info!("Stopping live ASR session");
    let _ = session.stop_tx.send(());
    let _ = session.join_handle.join();

    if let Ok(mut pending) = pending_paste_slot().lock() {
        if let Some(paste) = pending.take() {
            std::thread::sleep(Duration::from_millis(50));
            if let Err(err) = output::simulate_paste() {
                state::set_status_message(format!(
                    "Auto-paste unavailable, text is in clipboard: {}",
                    err
                ));
            }
            if let Some(backup) = paste.clipboard_backup {
                std::thread::sleep(Duration::from_millis(440));
                let _ = output::copy_to_clipboard(&backup);
            }
        }
    }

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
    selected_device: Option<String>,
    sample_tx: mpsc::Sender<Vec<f32>>,
    sample_rx: mpsc::Receiver<Vec<f32>>,
    stop_rx: mpsc::Receiver<()>,
) -> Result<(), String> {
    let mut capture = AudioCapture::new();
    capture
        .start(selected_device.as_deref(), sample_tx)
        .map_err(|e| e.to_string())?;

    let hotword_inventory = hotwords::load_all_enabled_with_paths(&paths).unwrap_or_else(|err| {
        log::warn!(
            "Failed to load hotword libraries, continuing without them: {}",
            err
        );
        Vec::new()
    });
    let text_config = TextProcessingConfig {
        punct_style: config.asr.punct_style.clone(),
        insert_punct: config.asr.insert_punct,
        comma_pause_ms: config.asr.comma_pause_ms,
        sentence_pause_ms: config.asr.sentence_pause_ms,
        hotwords: hotword_inventory.clone(),
    };

    let mut vad_detector: Option<VadDetector> = if config.audio.noise_reduction {
        let vad_model_dir = model::get_model_dir_with_paths(&paths, "silero-vad");
        let vad_model_path = vad_model_dir.join("silero_vad.onnx");
        match vad::ensure_vad_model(&vad_model_path) {
            Ok(()) => match VadDetector::new(
                &vad_model_path,
                config.audio.vad_threshold,
                config.audio.vad_end_threshold,
                config.audio.min_speech_frames,
                config.audio.silence_timeout_ms,
            ) {
                Ok(v) => Some(v),
                Err(e) => {
                    log::warn!("VAD init failed, running without VAD: {}", e);
                    None
                }
            },
            Err(e) => {
                log::warn!("VAD model setup failed, running without VAD: {}", e);
                None
            }
        }
    } else {
        None
    };
    log::info!(
        "Live ASR runtime initialized: vad_enabled={}, refine_enabled={}, vad_threshold={}, vad_end_threshold={}, min_speech_frames={}, silence_timeout_ms={}, comma_pause_ms={}, sentence_pause_ms={}",
        vad_detector.is_some(),
        config.asr.refine_enabled,
        config.audio.vad_threshold,
        config.audio.vad_end_threshold,
        config.audio.min_speech_frames,
        config.audio.silence_timeout_ms,
        config.asr.comma_pause_ms,
        config.asr.sentence_pause_ms
    );

    let mut live_engine = build_engine(
        &paths,
        &config,
        &text_config.hotwords,
        &config.asr.live_model_id,
    )?;
    let mut refine_worker = if config.asr.refine_enabled
        && model::is_model_downloaded_with_paths(&paths, &config.asr.refine_model_id)
    {
        Some(spawn_refine_worker(paths.clone(), config.clone())?)
    } else {
        if config.asr.refine_enabled {
            state::set_status_message(format!(
                "整体纠正已启用，但模型未就绪，当前仅使用实时模型 {}",
                config.asr.live_model_id
            ));
        }
        None
    };

    state::set_state(AppState::Recording);
    state::set_overlay_visible(true);
    state::set_status_message(format!(
        "Live ASR started with {}{}",
        config.asr.live_model_id,
        if refine_worker.is_some() {
            " + whole-model refine"
        } else {
            ""
        }
    ));
    state::clear_runtime_feedback();

    let started_at = Instant::now();
    let mut all_audio = Vec::new();
    let mut current_segment_audio = Vec::new();
    let mut segment_overlap_audio = Vec::new();
    let mut current_segment_leading_pause_ms = 0u32;
    let mut accumulated_silence_ms = 0u32;
    let mut pending_vad_boundary_pause_ms = 0u32;
    let mut pending_segment: Option<PendingSegment> = None;
    let mut current_partial = String::new();
    let mut segments: Vec<TranscriptSegment> = Vec::new();
    let mut next_segment_id = 1u64;

    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }

        if let Some(worker) = refine_worker.as_mut() {
            collect_refine_results(&worker.result_rx, &mut segments);
            sync_runtime_transcript(
                &text_config,
                &segments,
                pending_segment.as_ref(),
                &current_partial,
                current_segment_leading_pause_ms,
            );
        }

        match sample_rx.recv_timeout(Duration::from_millis(120)) {
            Ok(samples) => {
                let level = audio::calculate_audio_level(&samples);
                state::set_audio_level(level);
                all_audio.extend_from_slice(&samples);

                if let Some(ref mut detector) = vad_detector {
                    let events = match detector.process(&samples) {
                        Ok(events) => events,
                        Err(err) => {
                            log::warn!(
                                "VAD processing failed, disabling VAD for this session: {}",
                                err
                            );
                            state::set_status_message(format!(
                                "VAD 处理中断，已切换为连续识别: {}",
                                err
                            ));
                            vad_detector = None;
                            pending_vad_boundary_pause_ms = 0;
                            if let Err(err) = process_live_chunk(
                                &text_config,
                                &mut live_engine,
                                &samples,
                                &mut current_segment_audio,
                                &mut segment_overlap_audio,
                                &mut current_partial,
                                current_segment_leading_pause_ms,
                                &segments,
                                pending_segment.as_ref(),
                            ) {
                                recover_live_segment(
                                    &text_config,
                                    &mut live_engine,
                                    &mut current_segment_audio,
                                    &mut segment_overlap_audio,
                                    &mut current_segment_leading_pause_ms,
                                    &mut pending_segment,
                                    &mut current_partial,
                                    &mut segments,
                                    &mut next_segment_id,
                                    refine_worker.as_mut(),
                                    &err,
                                );
                            }
                            continue;
                        }
                    };
                    for event in events {
                        match event {
                            VadEvent::Speech(chunk) => {
                                log::debug!("VAD speech event: samples={}", chunk.len());
                                if current_segment_audio.is_empty() {
                                    current_segment_leading_pause_ms =
                                        effective_segment_leading_pause_ms(
                                            accumulated_silence_ms,
                                            pending_vad_boundary_pause_ms,
                                        );
                                    pending_vad_boundary_pause_ms = 0;
                                }
                                accumulated_silence_ms = 0;
                                if let Err(err) = process_live_chunk(
                                    &text_config,
                                    &mut live_engine,
                                    &chunk,
                                    &mut current_segment_audio,
                                    &mut segment_overlap_audio,
                                    &mut current_partial,
                                    current_segment_leading_pause_ms,
                                    &segments,
                                    pending_segment.as_ref(),
                                ) {
                                    recover_live_segment(
                                        &text_config,
                                        &mut live_engine,
                                        &mut current_segment_audio,
                                        &mut segment_overlap_audio,
                                        &mut current_segment_leading_pause_ms,
                                        &mut pending_segment,
                                        &mut current_partial,
                                        &mut segments,
                                        &mut next_segment_id,
                                        refine_worker.as_mut(),
                                        &err,
                                    );
                                }
                            }
                            VadEvent::Silence => {
                                accumulated_silence_ms = accumulated_silence_ms.saturating_add(32);
                                let had_active_segment = !current_segment_audio.is_empty()
                                    || !current_partial.is_empty();
                                if had_active_segment {
                                    log::info!(
                                        "VAD silence event: segment_samples={}, partial_len={}",
                                        current_segment_audio.len(),
                                        current_partial.len()
                                    );
                                }
                                if let Err(err) = finalize_segment(
                                    &text_config,
                                    &mut live_engine,
                                    &mut current_segment_audio,
                                    &mut segment_overlap_audio,
                                    &mut current_segment_leading_pause_ms,
                                    &mut pending_segment,
                                    &mut current_partial,
                                    &mut segments,
                                    &mut next_segment_id,
                                    refine_worker.as_mut(),
                                    false,
                                ) {
                                    recover_live_segment(
                                        &text_config,
                                        &mut live_engine,
                                        &mut current_segment_audio,
                                        &mut segment_overlap_audio,
                                        &mut current_segment_leading_pause_ms,
                                        &mut pending_segment,
                                        &mut current_partial,
                                        &mut segments,
                                        &mut next_segment_id,
                                        refine_worker.as_mut(),
                                        &err,
                                    );
                                } else if had_active_segment {
                                    pending_vad_boundary_pause_ms = config.audio.silence_timeout_ms;
                                }
                            }
                        }
                    }
                } else {
                    if let Err(err) = process_live_chunk(
                        &text_config,
                        &mut live_engine,
                        &samples,
                        &mut current_segment_audio,
                        &mut segment_overlap_audio,
                        &mut current_partial,
                        current_segment_leading_pause_ms,
                        &segments,
                        pending_segment.as_ref(),
                    ) {
                        recover_live_segment(
                            &text_config,
                            &mut live_engine,
                            &mut current_segment_audio,
                            &mut segment_overlap_audio,
                            &mut current_segment_leading_pause_ms,
                            &mut pending_segment,
                            &mut current_partial,
                            &mut segments,
                            &mut next_segment_id,
                            refine_worker.as_mut(),
                            &err,
                        );
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    capture.stop();

    if let Some(ref mut detector) = vad_detector {
        detector.reset();
    }

    finalize_segment(
        &text_config,
        &mut live_engine,
        &mut current_segment_audio,
        &mut segment_overlap_audio,
        &mut current_segment_leading_pause_ms,
        &mut pending_segment,
        &mut current_partial,
        &mut segments,
        &mut next_segment_id,
        refine_worker.as_mut(),
        true,
    )?;

    if let Some(worker) = refine_worker {
        drop(worker.task_tx);
        let _ = worker.join_handle.join();
        collect_refine_results_blocking(&worker.result_rx, &mut segments);
    }

    let live_transcribed = finalize_transcript_text(
        &compose_live_transcript(&text_config, &segments, pending_segment.as_ref()),
        &text_config,
    );
    let corrected_transcribed = finalize_transcript_text(
        &compose_transcript(
            &text_config,
            &segments,
            pending_segment.as_ref(),
            "",
            0,
            true,
        ),
        &text_config,
    );
    let final_transcribed = if corrected_transcribed.is_empty() {
        live_transcribed.clone()
    } else {
        corrected_transcribed.clone()
    };

    if !final_transcribed.is_empty() {
        state::set_last_transcript(final_transcribed.clone());

        let rewritten_text = maybe_rewrite_text(&config, &final_transcribed);
        if let Some(ref rewritten) = rewritten_text {
            state::set_rewrite_preview(rewritten.clone());
        }

        let output_source = rewritten_text
            .clone()
            .unwrap_or_else(|| final_transcribed.clone());
        let formatted_text = output::format_output(&output_source, &config.output);

        let clipboard_backup = if config.output.restore_clipboard {
            output::read_clipboard_text().ok()
        } else {
            None
        };

        match output::copy_to_clipboard(&formatted_text) {
            Ok(()) => {
                state::set_final_output(formatted_text.clone());
                if let Ok(mut pending) = pending_paste_slot().lock() {
                    *pending = Some(PendingPaste { clipboard_backup });
                }
            }
            Err(err) => {
                state::set_final_output(formatted_text);
                state::set_status_message(format!("Clipboard copy failed: {}", err));
            }
        }

        let audio_path = save_recording_audio(&paths, &all_audio)?;
        save_final_history(
            &paths,
            &config,
            &live_transcribed,
            &corrected_transcribed,
            rewritten_text.as_deref(),
            audio_path.as_deref(),
            started_at.elapsed(),
        )?;
    }

    state::set_live_transcript("");
    state::set_audio_level(0.0);
    state::set_overlay_visible(false);
    state::set_state(AppState::Idle);
    state::set_status_message(if final_transcribed.is_empty() {
        "Live ASR stopped with no final transcript"
    } else {
        "Live ASR stopped, output delivered"
    });

    Ok(())
}

fn build_engine(
    paths: &AppPaths,
    config: &AppConfig,
    hotwords: &[shanji_core::hotwords::Hotword],
    model_id: &str,
) -> Result<AsrEngine, String> {
    let mut engine = AsrEngine::new(AsrConfig {
        insert_punct: config.asr.insert_punct,
        punct_style: config.asr.punct_style.clone(),
        hotwords: hotwords
            .iter()
            .map(|word| (word.word.clone(), word.weight))
            .collect(),
        ..AsrConfig::default()
    })
    .map_err(|e| e.to_string())?;
    let model_layout =
        model::resolve_model_layout_with_paths(paths, model_id).map_err(|e| e.to_string())?;
    engine
        .load_model(&model_layout)
        .map_err(|e| e.to_string())?;
    Ok(engine)
}

fn effective_segment_leading_pause_ms(accumulated_silence_ms: u32, boundary_pause_ms: u32) -> u32 {
    accumulated_silence_ms.saturating_add(boundary_pause_ms)
}

fn spawn_refine_worker(paths: AppPaths, config: AppConfig) -> Result<RefineWorker, String> {
    let (task_tx, task_rx) = mpsc::channel::<RefineTask>();
    let (result_tx, result_rx) = mpsc::channel::<RefineResult>();
    let join_handle = std::thread::spawn(move || {
        let hotwords = hotwords::load_all_enabled_with_paths(&paths).unwrap_or_default();
        let mut engine = match build_engine(&paths, &config, &hotwords, &config.asr.refine_model_id)
        {
            Ok(engine) => engine,
            Err(err) => {
                log::warn!("Failed to start refine worker: {}", err);
                return;
            }
        };

        while let Ok(task) = task_rx.recv() {
            let result_text = match refine_segment(&mut engine, &task.audio) {
                Ok(text) if !text.is_empty() => text,
                Ok(_) | Err(_) => task.fallback_text,
            };

            if result_tx
                .send(RefineResult {
                    segment_id: task.segment_id,
                    text: result_text,
                })
                .is_err()
            {
                break;
            }
        }
    });

    Ok(RefineWorker {
        task_tx,
        result_rx,
        join_handle,
    })
}

fn refine_segment(engine: &mut AsrEngine, audio: &[f32]) -> Result<String, String> {
    let _ = engine.process_chunk(audio).map_err(|e| e.to_string())?;
    engine.finalize().map_err(|e| e.to_string())
}

fn process_live_chunk(
    text_config: &TextProcessingConfig,
    live_engine: &mut AsrEngine,
    samples: &[f32],
    current_segment_audio: &mut Vec<f32>,
    segment_overlap_audio: &mut Vec<f32>,
    current_partial: &mut String,
    current_partial_leading_pause_ms: u32,
    segments: &[TranscriptSegment],
    pending_segment: Option<&PendingSegment>,
) -> Result<(), String> {
    if samples.is_empty() {
        return Ok(());
    }

    if current_segment_audio.is_empty() && !segment_overlap_audio.is_empty() {
        current_segment_audio.extend_from_slice(segment_overlap_audio);
        segment_overlap_audio.clear();
    }

    current_segment_audio.extend_from_slice(samples);
    match live_engine
        .process_chunk(samples)
        .map_err(|e| e.to_string())?
    {
        Some(text) if !text.is_empty() => {
            *current_partial = text;
            sync_runtime_transcript(
                text_config,
                segments,
                pending_segment,
                current_partial,
                current_partial_leading_pause_ms,
            );
            state::set_state(AppState::Transcribing);
            state::set_status_message("Live ASR is producing partial text");
            log::info!(
                "Live ASR partial text updated: samples={}, text='{}'",
                samples.len(),
                current_partial
            );
        }
        _ => {
            log::debug!(
                "Live ASR chunk accepted with no partial text: samples={}, segment_samples={}",
                samples.len(),
                current_segment_audio.len()
            );
        }
    }

    Ok(())
}

fn finalize_segment(
    text_config: &TextProcessingConfig,
    live_engine: &mut AsrEngine,
    current_segment_audio: &mut Vec<f32>,
    segment_overlap_audio: &mut Vec<f32>,
    current_segment_leading_pause_ms: &mut u32,
    pending_segment: &mut Option<PendingSegment>,
    current_partial: &mut String,
    segments: &mut Vec<TranscriptSegment>,
    next_segment_id: &mut u64,
    refine_worker: Option<&mut RefineWorker>,
    flush_short_segments: bool,
) -> Result<(), String> {
    if current_segment_audio.is_empty() && current_partial.is_empty() {
        if flush_short_segments {
            commit_pending_segment(pending_segment, segments, next_segment_id, refine_worker)?;
            sync_runtime_transcript(text_config, segments, pending_segment.as_ref(), "", 0);
        }
        return Ok(());
    }

    let live_text = live_engine.finalize().map_err(|e| e.to_string())?;
    let mut live_text = if live_text.is_empty() {
        current_partial.clone()
    } else {
        live_text
    };

    current_partial.clear();
    if live_text.is_empty() {
        log::info!(
            "Segment finalize produced empty text: segment_samples={}, flush_short_segments={}",
            current_segment_audio.len(),
            flush_short_segments
        );
        current_segment_audio.clear();
        *current_segment_leading_pause_ms = 0;
        sync_runtime_transcript(text_config, segments, pending_segment.as_ref(), "", 0);
        return Ok(());
    }

    let overlap_len = current_segment_audio.len().min(SEGMENT_OVERLAP_SAMPLES);
    *segment_overlap_audio =
        current_segment_audio[current_segment_audio.len() - overlap_len..].to_vec();

    let mut segment_audio = std::mem::take(current_segment_audio);
    if let Some(previous) = pending_segment.take() {
        let mut merged_audio = previous.audio;
        merged_audio.extend_from_slice(&segment_audio);
        segment_audio = merged_audio;
        live_text = format!("{}{}", previous.live_text, live_text);
        *current_segment_leading_pause_ms = previous.leading_pause_ms;
    }

    if !flush_short_segments && segment_audio.len() < MIN_REFINE_SEGMENT_SAMPLES {
        log::info!(
            "Segment kept pending for merge: samples={}, text='{}'",
            segment_audio.len(),
            live_text
        );
        *pending_segment = Some(PendingSegment {
            leading_pause_ms: *current_segment_leading_pause_ms,
            audio: segment_audio,
            live_text,
        });
        *current_segment_leading_pause_ms = 0;
        sync_runtime_transcript(text_config, segments, pending_segment.as_ref(), "", 0);
        return Ok(());
    }

    push_segment(
        segments,
        next_segment_id,
        refine_worker,
        *current_segment_leading_pause_ms,
        segment_audio,
        live_text,
    );
    *current_segment_leading_pause_ms = 0;
    if let Some(segment) = segments.last() {
        log::info!(
            "Segment committed: id={}, pause_ms={}, text='{}', corrected={}",
            segment.id,
            segment.leading_pause_ms,
            segment.live_text,
            segment.corrected_text.is_some()
        );
    }
    sync_runtime_transcript(text_config, segments, pending_segment.as_ref(), "", 0);
    Ok(())
}

fn commit_pending_segment(
    pending_segment: &mut Option<PendingSegment>,
    segments: &mut Vec<TranscriptSegment>,
    next_segment_id: &mut u64,
    refine_worker: Option<&mut RefineWorker>,
) -> Result<(), String> {
    let Some(pending) = pending_segment.take() else {
        return Ok(());
    };

    push_segment(
        segments,
        next_segment_id,
        refine_worker,
        pending.leading_pause_ms,
        pending.audio,
        pending.live_text,
    );
    Ok(())
}

fn push_segment(
    segments: &mut Vec<TranscriptSegment>,
    next_segment_id: &mut u64,
    refine_worker: Option<&mut RefineWorker>,
    leading_pause_ms: u32,
    segment_audio: Vec<f32>,
    live_text: String,
) {
    let segment_id = *next_segment_id;
    *next_segment_id += 1;
    segments.push(TranscriptSegment {
        id: segment_id,
        leading_pause_ms,
        live_text: live_text.clone(),
        corrected_text: None,
    });
    log::info!(
        "Queued transcript segment: id={}, audio_samples={}, text='{}', refine_worker={}",
        segment_id,
        segment_audio.len(),
        live_text,
        refine_worker.is_some()
    );

    if let Some(worker) = refine_worker {
        let _ = worker.task_tx.send(RefineTask {
            segment_id,
            audio: segment_audio,
            fallback_text: live_text,
        });
    }
}

fn collect_refine_results(
    result_rx: &mpsc::Receiver<RefineResult>,
    segments: &mut [TranscriptSegment],
) {
    while let Ok(result) = result_rx.try_recv() {
        apply_refine_result(segments, result);
    }
}

fn collect_refine_results_blocking(
    result_rx: &mpsc::Receiver<RefineResult>,
    segments: &mut [TranscriptSegment],
) {
    while let Ok(result) = result_rx.recv_timeout(Duration::from_millis(20)) {
        apply_refine_result(segments, result);
    }
}

fn apply_refine_result(segments: &mut [TranscriptSegment], result: RefineResult) {
    if let Some(segment) = segments
        .iter_mut()
        .find(|segment| segment.id == result.segment_id)
    {
        log::info!(
            "Refine result applied: id={}, text='{}'",
            result.segment_id,
            result.text
        );
        segment.corrected_text = Some(result.text);
    }
}

fn compose_live_transcript(
    text_config: &TextProcessingConfig,
    segments: &[TranscriptSegment],
    pending_segment: Option<&PendingSegment>,
) -> String {
    render_segmented_transcript(
        &segments
            .iter()
            .map(|segment| TranscriptChunk {
                text: segment.live_text.as_str(),
                leading_pause_ms: segment.leading_pause_ms,
            })
            .collect::<Vec<_>>(),
        pending_segment.map(|segment| TranscriptChunk {
            text: segment.live_text.as_str(),
            leading_pause_ms: segment.leading_pause_ms,
        }),
        None,
        text_config,
        false,
    )
}

fn compose_transcript(
    text_config: &TextProcessingConfig,
    segments: &[TranscriptSegment],
    pending_segment: Option<&PendingSegment>,
    current_partial: &str,
    current_partial_leading_pause_ms: u32,
    is_final: bool,
) -> String {
    render_segmented_transcript(
        &segments
            .iter()
            .map(|segment| TranscriptChunk {
                text: segment
                    .corrected_text
                    .as_deref()
                    .unwrap_or(segment.live_text.as_str()),
                leading_pause_ms: segment.leading_pause_ms,
            })
            .collect::<Vec<_>>(),
        pending_segment.map(|segment| TranscriptChunk {
            text: segment.live_text.as_str(),
            leading_pause_ms: segment.leading_pause_ms,
        }),
        (!current_partial.is_empty()).then_some(TranscriptChunk {
            text: current_partial,
            leading_pause_ms: current_partial_leading_pause_ms,
        }),
        text_config,
        is_final,
    )
}

fn sync_runtime_transcript(
    text_config: &TextProcessingConfig,
    segments: &[TranscriptSegment],
    pending_segment: Option<&PendingSegment>,
    current_partial: &str,
    current_partial_leading_pause_ms: u32,
) {
    let display = compose_transcript(
        text_config,
        segments,
        pending_segment,
        current_partial,
        current_partial_leading_pause_ms,
        false,
    );
    state::set_live_transcript(display.clone());
    state::set_last_transcript(display.clone());
    log::info!(
        "Runtime transcript synced: segments={}, pending={}, partial_len={}, display='{}'",
        segments.len(),
        pending_segment.is_some(),
        current_partial.len(),
        display
    );
    if !current_partial.is_empty()
        || pending_segment.is_some()
        || segments.iter().any(|segment| !segment.live_text.is_empty())
    {
        state::set_state(AppState::Transcribing);
    }
}

fn recover_live_segment(
    text_config: &TextProcessingConfig,
    live_engine: &mut AsrEngine,
    current_segment_audio: &mut Vec<f32>,
    segment_overlap_audio: &mut Vec<f32>,
    current_segment_leading_pause_ms: &mut u32,
    pending_segment: &mut Option<PendingSegment>,
    current_partial: &mut String,
    segments: &mut Vec<TranscriptSegment>,
    next_segment_id: &mut u64,
    refine_worker: Option<&mut RefineWorker>,
    err: &str,
) {
    log::warn!("Live ASR chunk failed, resetting active segment: {}", err);
    let mut fallback_text = current_partial.trim().to_string();
    let mut segment_audio = std::mem::take(current_segment_audio);
    let mut leading_pause_ms = *current_segment_leading_pause_ms;

    live_engine.reset();
    segment_overlap_audio.clear();
    current_partial.clear();

    if !fallback_text.is_empty() {
        if let Some(previous) = pending_segment.take() {
            let mut merged_audio = previous.audio;
            merged_audio.extend_from_slice(&segment_audio);
            segment_audio = merged_audio;
            leading_pause_ms = previous.leading_pause_ms;
            fallback_text = format!("{}{}", previous.live_text, fallback_text);
        }

        if segment_audio.len() < MIN_REFINE_SEGMENT_SAMPLES {
            log::info!(
                "Recovered live segment as pending text after failure: samples={}, text='{}'",
                segment_audio.len(),
                fallback_text
            );
            *pending_segment = Some(PendingSegment {
                leading_pause_ms,
                audio: segment_audio,
                live_text: fallback_text,
            });
        } else {
            log::info!(
                "Recovered live segment as committed text after failure: samples={}, text='{}'",
                segment_audio.len(),
                fallback_text
            );
            push_segment(
                segments,
                next_segment_id,
                refine_worker,
                leading_pause_ms,
                segment_audio,
                fallback_text,
            );
        }
    }

    *current_segment_leading_pause_ms = 0;
    sync_runtime_transcript(text_config, segments, pending_segment.as_ref(), "", 0);
    state::set_state(AppState::Recording);
    state::set_status_message(format!("实时转写分段异常，已自动恢复: {}", err));
}

fn maybe_rewrite_text(config: &AppConfig, text: &str) -> Option<String> {
    if !config.rewrite.enabled || config.rewrite.active_provider_id.is_empty() {
        return None;
    }

    state::set_state(AppState::Rewriting);
    state::set_status_message("正在运行 LLM 润色");

    match llm::create_client(&config.rewrite.active_provider_id, &config.rewrite) {
        Ok(client) => match client.rewrite(text) {
            Ok(rewritten) if !rewritten.is_empty() => Some(rewritten),
            Ok(_) => None,
            Err(err) => {
                state::set_status_message(format!("LLM 润色失败，已保留原文: {}", err));
                None
            }
        },
        Err(err) => {
            state::set_status_message(format!("LLM 润色不可用，已保留原文: {}", err));
            None
        }
    }
}

fn save_recording_audio(paths: &AppPaths, samples: &[f32]) -> Result<Option<String>, String> {
    if samples.is_empty() {
        return Ok(None);
    }

    std::fs::create_dir_all(paths.recordings_dir()).map_err(|e| e.to_string())?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let path = paths
        .recordings_dir()
        .join(format!("shanji-recording-{}.wav", timestamp));

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut writer = hound::WavWriter::create(&path, spec).map_err(|e| e.to_string())?;
    for sample in samples {
        let pcm = (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
        writer.write_sample(pcm).map_err(|e| e.to_string())?;
    }
    writer.finalize().map_err(|e| e.to_string())?;

    Ok(Some(path.display().to_string()))
}

fn save_final_history(
    paths: &AppPaths,
    config: &AppConfig,
    live_transcribed: &str,
    corrected_transcribed: &str,
    rewritten: Option<&str>,
    audio_path: Option<&str>,
    duration: Duration,
) -> Result<(), String> {
    let db = HistoryDb::new_with_paths(paths).map_err(|e| e.to_string())?;
    let final_transcribed = if corrected_transcribed.is_empty() {
        live_transcribed.to_string()
    } else {
        corrected_transcribed.to_string()
    };
    let record = HistoryRecord {
        id: None,
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        transcribed: final_transcribed,
        live_transcribed: Some(live_transcribed.to_string()),
        corrected_transcribed: if corrected_transcribed.is_empty() {
            None
        } else {
            Some(corrected_transcribed.to_string())
        },
        rewritten: rewritten.map(|text| text.to_string()),
        duration_ms: Some(duration.as_millis().min(u32::MAX as u128) as u32),
        model_id: Some(config.asr.live_model_id.clone()),
        live_model_id: Some(config.asr.live_model_id.clone()),
        refine_model_id: if config.asr.refine_enabled {
            Some(config.asr.refine_model_id.clone())
        } else {
            None
        },
        refine_enabled: config.asr.refine_enabled,
        provider_id: rewritten.map(|_| config.rewrite.active_provider_id.clone()),
        audio_path: audio_path.map(|path| path.to_string()),
    };
    db.insert(&record).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::effective_segment_leading_pause_ms;

    #[test]
    fn leading_pause_includes_vad_boundary_credit() {
        assert_eq!(effective_segment_leading_pause_ms(32, 1500), 1532);
    }

    #[test]
    fn leading_pause_saturates_on_overflow() {
        assert_eq!(
            effective_segment_leading_pause_ms(u32::MAX - 5, 32),
            u32::MAX
        );
    }
}

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
    source_id: Option<u64>,
    source_clause_index: Option<usize>,
    leading_pause_ms: u32,
    live_text: String,
    corrected_text: Option<String>,
}

struct RefineTask {
    source_id: u64,
    audio: Vec<f32>,
    fallback_text: String,
    clause_count: usize,
    is_final: bool,
}

struct RefineResult {
    source_id: u64,
    text: String,
    clause_count: usize,
    is_final: bool,
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

#[derive(Clone, Debug)]
struct SourceClauseContext {
    text: String,
    leading_pause_ms: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RefineAudioStats {
    duration_ms: u32,
    low_energy_frame_count: usize,
    low_energy_run_count: usize,
    longest_low_energy_ms: u32,
    longest_internal_low_energy_ms: u32,
    trailing_low_energy_ms: u32,
}

struct ActiveRefineContext {
    source_id: u64,
    leading_pause_ms: u32,
    committed_clause_count: usize,
    queued_clause_count: usize,
    source_audio: Vec<f32>,
    pending_clause_leading_pause_ms: u32,
    dormant: bool,
}

const STRONG_CONTINUATION_MARKERS: &[&str] = &[
    "适合",
    "更适合",
    "适用于",
    "用于",
    "支持",
    "可以",
    "能够",
    "需要",
    "值得",
    "便于",
];

const SEGMENT_OVERLAP_SAMPLES: usize = 3_200;
const MIN_REFINE_SEGMENT_SAMPLES: usize = 16_000;
const REFINE_AUDIO_ANALYSIS_FRAME_SAMPLES: usize = 512;
const REFINE_AUDIO_LOW_ENERGY_RMS_THRESHOLD: f32 = 0.01;

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
            " + incremental refine"
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
    let mut active_refine: Option<ActiveRefineContext> = None;
    let mut current_partial = String::new();
    let mut segments: Vec<TranscriptSegment> = Vec::new();
    let mut next_segment_id = 1u64;
    let mut next_refine_source_id = 1u64;

    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }

        if let Some(worker) = refine_worker.as_mut() {
            if collect_refine_results(&text_config, &worker.result_rx, &mut segments) {
                let tail = if current_partial.is_empty() {
                    String::new()
                } else if let Some(active) = active_refine.as_ref() {
                    let source_clauses = source_clause_contexts(&segments, active.source_id);
                    let display = build_refine_display_with_source_context(
                        &text_config,
                        &source_clauses,
                        active.committed_clause_count,
                        &current_partial,
                        false,
                    );
                    remaining_display_after_clause_count(&display, active.committed_clause_count)
                } else {
                    current_partial.clone()
                };
                sync_runtime_transcript(
                    &text_config,
                    &segments,
                    None,
                    &tail,
                    tail_leading_pause_ms(active_refine.as_ref()),
                );
            }
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
                            let result = if let Some(worker) = refine_worker.as_mut() {
                                process_live_chunk_with_incremental_refine(
                                    &text_config,
                                    &mut live_engine,
                                    &samples,
                                    &mut current_segment_audio,
                                    &mut segment_overlap_audio,
                                    &mut current_partial,
                                    current_segment_leading_pause_ms,
                                    &mut active_refine,
                                    &mut next_refine_source_id,
                                    &mut segments,
                                    &mut next_segment_id,
                                    worker,
                                )
                            } else {
                                process_live_chunk(
                                    &text_config,
                                    &mut live_engine,
                                    &samples,
                                    &mut current_segment_audio,
                                    &mut segment_overlap_audio,
                                    &mut current_partial,
                                    current_segment_leading_pause_ms,
                                    &segments,
                                    pending_segment.as_ref(),
                                )
                            };
                            if let Err(err) = result {
                                if refine_worker.is_some() {
                                    recover_live_segment_with_incremental_refine(
                                        &text_config,
                                        &mut live_engine,
                                        &mut current_segment_audio,
                                        &mut segment_overlap_audio,
                                        &mut current_segment_leading_pause_ms,
                                        &mut active_refine,
                                        &mut next_refine_source_id,
                                        &mut current_partial,
                                        &mut segments,
                                        &mut next_segment_id,
                                        &err,
                                    );
                                } else {
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
                                let result = if let Some(worker) = refine_worker.as_mut() {
                                    process_live_chunk_with_incremental_refine(
                                        &text_config,
                                        &mut live_engine,
                                        &chunk,
                                        &mut current_segment_audio,
                                        &mut segment_overlap_audio,
                                        &mut current_partial,
                                        current_segment_leading_pause_ms,
                                        &mut active_refine,
                                        &mut next_refine_source_id,
                                        &mut segments,
                                        &mut next_segment_id,
                                        worker,
                                    )
                                } else {
                                    process_live_chunk(
                                        &text_config,
                                        &mut live_engine,
                                        &chunk,
                                        &mut current_segment_audio,
                                        &mut segment_overlap_audio,
                                        &mut current_partial,
                                        current_segment_leading_pause_ms,
                                        &segments,
                                        pending_segment.as_ref(),
                                    )
                                };
                                if let Err(err) = result {
                                    if refine_worker.is_some() {
                                        recover_live_segment_with_incremental_refine(
                                            &text_config,
                                            &mut live_engine,
                                            &mut current_segment_audio,
                                            &mut segment_overlap_audio,
                                            &mut current_segment_leading_pause_ms,
                                            &mut active_refine,
                                            &mut next_refine_source_id,
                                            &mut current_partial,
                                            &mut segments,
                                            &mut next_segment_id,
                                            &err,
                                        );
                                    } else {
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
                            VadEvent::Silence => {
                                accumulated_silence_ms = accumulated_silence_ms.saturating_add(32);
                                let had_active_segment = !current_segment_audio.is_empty()
                                    || !current_partial.is_empty();
                                if !had_active_segment {
                                    continue;
                                }
                                if had_active_segment {
                                    log::info!(
                                        "VAD silence event: segment_samples={}, partial_len={}",
                                        current_segment_audio.len(),
                                        current_partial.len()
                                    );
                                }
                                let result = if let Some(worker) = refine_worker.as_mut() {
                                    finalize_segment_with_incremental_refine(
                                        &text_config,
                                        &mut live_engine,
                                        &mut current_segment_audio,
                                        &mut segment_overlap_audio,
                                        &mut current_segment_leading_pause_ms,
                                        &mut active_refine,
                                        &mut next_refine_source_id,
                                        &mut current_partial,
                                        &mut segments,
                                        &mut next_segment_id,
                                        worker,
                                    )
                                } else {
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
                                        false,
                                    )
                                };
                                if let Err(err) = result {
                                    if refine_worker.is_some() {
                                        recover_live_segment_with_incremental_refine(
                                            &text_config,
                                            &mut live_engine,
                                            &mut current_segment_audio,
                                            &mut segment_overlap_audio,
                                            &mut current_segment_leading_pause_ms,
                                            &mut active_refine,
                                            &mut next_refine_source_id,
                                            &mut current_partial,
                                            &mut segments,
                                            &mut next_segment_id,
                                            &err,
                                        );
                                    } else {
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
                                } else if had_active_segment {
                                    pending_vad_boundary_pause_ms = config.audio.silence_timeout_ms;
                                }
                            }
                        }
                    }
                } else {
                    let result = if let Some(worker) = refine_worker.as_mut() {
                        process_live_chunk_with_incremental_refine(
                            &text_config,
                            &mut live_engine,
                            &samples,
                            &mut current_segment_audio,
                            &mut segment_overlap_audio,
                            &mut current_partial,
                            current_segment_leading_pause_ms,
                            &mut active_refine,
                            &mut next_refine_source_id,
                            &mut segments,
                            &mut next_segment_id,
                            worker,
                        )
                    } else {
                        process_live_chunk(
                            &text_config,
                            &mut live_engine,
                            &samples,
                            &mut current_segment_audio,
                            &mut segment_overlap_audio,
                            &mut current_partial,
                            current_segment_leading_pause_ms,
                            &segments,
                            pending_segment.as_ref(),
                        )
                    };
                    if let Err(err) = result {
                        if refine_worker.is_some() {
                            recover_live_segment_with_incremental_refine(
                                &text_config,
                                &mut live_engine,
                                &mut current_segment_audio,
                                &mut segment_overlap_audio,
                                &mut current_segment_leading_pause_ms,
                                &mut active_refine,
                                &mut next_refine_source_id,
                                &mut current_partial,
                                &mut segments,
                                &mut next_segment_id,
                                &err,
                            );
                        } else {
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
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    capture.stop();

    if let Some(ref mut detector) = vad_detector {
        detector.reset();
    }

    if let Some(worker) = refine_worker.as_mut() {
        finalize_segment_with_incremental_refine(
            &text_config,
            &mut live_engine,
            &mut current_segment_audio,
            &mut segment_overlap_audio,
            &mut current_segment_leading_pause_ms,
            &mut active_refine,
            &mut next_refine_source_id,
            &mut current_partial,
            &mut segments,
            &mut next_segment_id,
            worker,
        )?;
    } else {
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
    }

    if let Some(worker) = refine_worker {
        drop(worker.task_tx);
        let _ = worker.join_handle.join();
        collect_refine_results_blocking(&text_config, &worker.result_rx, &mut segments);
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
            let RefineTask {
                source_id,
                audio,
                fallback_text,
                clause_count,
                is_final,
            } = task;
            let refine_output = refine_segment(&mut engine, &audio);
            let (result_text, raw_output, used_fallback) = match refine_output {
                Ok(text) if !text.is_empty() => (text.clone(), text, false),
                Ok(text) => (fallback_text.clone(), text, true),
                Err(err) => {
                    log::warn!(
                        "Refine worker failed: source_id={}, final={}, clause_count={}, error={}",
                        source_id,
                        is_final,
                        clause_count,
                        err
                    );
                    (fallback_text.clone(), String::new(), true)
                }
            };
            log::info!(
                "Refine worker output: source_id={}, final={}, clause_count={}, fallback='{}', raw='{}', chosen='{}', used_fallback={}",
                source_id,
                is_final,
                clause_count,
                fallback_text,
                raw_output,
                result_text,
                used_fallback
            );

            if result_tx
                .send(RefineResult {
                    source_id,
                    text: result_text,
                    clause_count,
                    is_final,
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

fn source_clause_contexts(
    segments: &[TranscriptSegment],
    source_id: u64,
) -> Vec<SourceClauseContext> {
    let mut clauses = segments
        .iter()
        .filter_map(|segment| {
            (segment.source_id == Some(source_id))
                .then_some(segment.source_clause_index)
                .flatten()
                .map(|index| {
                    (
                        index,
                        SourceClauseContext {
                            text: segment
                                .corrected_text
                                .clone()
                                .unwrap_or_else(|| segment.live_text.clone()),
                            leading_pause_ms: segment.leading_pause_ms,
                        },
                    )
                })
        })
        .collect::<Vec<_>>();
    clauses.sort_by_key(|(index, _)| *index);
    clauses.into_iter().map(|(_, clause)| clause).collect()
}

fn summarize_source_clause_pauses(segments: &[TranscriptSegment], source_id: u64) -> String {
    let clauses = source_clause_contexts(segments, source_id);
    if clauses.is_empty() {
        return "-".to_string();
    }

    clauses
        .iter()
        .enumerate()
        .map(|(index, clause)| format!("{}:{}ms", index, clause.leading_pause_ms))
        .collect::<Vec<_>>()
        .join(",")
}

fn analyze_refine_audio(audio: &[f32]) -> RefineAudioStats {
    let duration_ms = ((audio.len() as u64) * 1000 / 16_000) as u32;
    let mut low_energy_frame_count = 0usize;
    let mut low_energy_runs = Vec::new();
    let mut current_low_energy_run = 0usize;

    let low_energy_flags = audio
        .chunks(REFINE_AUDIO_ANALYSIS_FRAME_SAMPLES)
        .filter(|frame| !frame.is_empty())
        .map(|frame| {
            let energy =
                frame.iter().map(|sample| sample * sample).sum::<f32>() / frame.len() as f32;
            energy.sqrt() <= REFINE_AUDIO_LOW_ENERGY_RMS_THRESHOLD
        })
        .collect::<Vec<_>>();

    for &is_low_energy in &low_energy_flags {
        if is_low_energy {
            low_energy_frame_count += 1;
            current_low_energy_run += 1;
        } else if current_low_energy_run != 0 {
            low_energy_runs.push(current_low_energy_run);
            current_low_energy_run = 0;
        }
    }

    let trailing_low_energy_run = if current_low_energy_run != 0 {
        low_energy_runs.push(current_low_energy_run);
        current_low_energy_run
    } else {
        0
    };
    let longest_low_energy_run = low_energy_runs.iter().copied().max().unwrap_or(0);
    let longest_internal_low_energy_run = if trailing_low_energy_run != 0 {
        low_energy_runs
            .iter()
            .copied()
            .take(low_energy_runs.len().saturating_sub(1))
            .max()
            .unwrap_or(0)
    } else {
        longest_low_energy_run
    };

    RefineAudioStats {
        duration_ms,
        low_energy_frame_count,
        low_energy_run_count: low_energy_runs.len(),
        longest_low_energy_ms: (longest_low_energy_run as u32) * 32,
        longest_internal_low_energy_ms: (longest_internal_low_energy_run as u32) * 32,
        trailing_low_energy_ms: (trailing_low_energy_run as u32) * 32,
    }
}

fn boundaryless_clause_text(clauses: &[SourceClauseContext]) -> String {
    let mut text = String::new();
    for clause in clauses {
        text.push_str(&strip_terminal_boundary_punctuation(&clause.text));
    }
    text
}

fn remaining_text_after_prefix(prefix: &str, text: &str) -> String {
    if prefix.is_empty() {
        return text.to_string();
    }

    if text.starts_with(prefix) {
        return text.chars().skip(prefix.chars().count()).collect();
    }

    let overlap_chars = prefix
        .chars()
        .zip(text.chars())
        .take_while(|(left, right)| left == right)
        .count();
    text.chars().skip(overlap_chars).collect()
}

fn build_refine_display_with_source_context(
    text_config: &TextProcessingConfig,
    source_clauses: &[SourceClauseContext],
    anchor_clause_count: usize,
    text: &str,
    is_final: bool,
) -> String {
    let anchor_clause_count = anchor_clause_count.min(source_clauses.len());
    let committed_clauses = &source_clauses[..anchor_clause_count];
    let committed_prefix = boundaryless_clause_text(committed_clauses);
    let remaining_text = remaining_text_after_prefix(&committed_prefix, text);
    let committed_chunks = committed_clauses
        .iter()
        .map(|clause| TranscriptChunk {
            text: clause.text.as_str(),
            leading_pause_ms: clause.leading_pause_ms,
        })
        .collect::<Vec<_>>();
    let next_leading_pause_ms = source_clauses
        .get(anchor_clause_count)
        .map(|clause| clause.leading_pause_ms)
        .unwrap_or(0);

    render_segmented_transcript(
        &committed_chunks,
        None,
        (!remaining_text.is_empty()).then_some(TranscriptChunk {
            text: remaining_text.as_str(),
            leading_pause_ms: next_leading_pause_ms,
        }),
        text_config,
        is_final,
    )
}

fn split_stable_clauses(display: &str) -> (Vec<String>, String) {
    let mut clauses = Vec::new();
    let mut current = String::new();

    for ch in display.chars() {
        current.push(ch);
        if matches!(ch, '，' | '。' | '！' | '？' | '；') {
            let clause = current.trim().to_string();
            if !clause.is_empty() {
                clauses.push(clause);
            }
            current.clear();
        }
    }

    (clauses, current.trim().to_string())
}

fn is_boundary_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '，' | '。' | '！' | '？' | '；' | ',' | '.' | '!' | '?' | ';'
    )
}

fn strip_terminal_boundary_punctuation(text: &str) -> String {
    text.trim_end_matches(is_boundary_punctuation).to_string()
}

fn remaining_display_after_clause_count(display: &str, clause_count: usize) -> String {
    let (clauses, tail) = split_stable_clauses(display);
    let mut remaining = clauses.into_iter().skip(clause_count).collect::<String>();
    remaining.push_str(&tail);
    remaining
}

fn tail_leading_pause_ms(active_refine: Option<&ActiveRefineContext>) -> u32 {
    active_refine
        .map(|context| {
            if context.committed_clause_count > 0 {
                0
            } else {
                context.leading_pause_ms
            }
        })
        .unwrap_or(0)
}

fn activate_refine_context<'a>(
    active_refine: &'a mut Option<ActiveRefineContext>,
    next_refine_source_id: &mut u64,
    leading_pause_ms: u32,
    _sentence_pause_ms: u32,
    _segments: &mut [TranscriptSegment],
) -> &'a mut ActiveRefineContext {
    if matches!(active_refine.as_ref(), Some(context) if !context.dormant) {
        return active_refine
            .as_mut()
            .expect("active refine context must exist");
    }

    let source_id = *next_refine_source_id;
    *next_refine_source_id += 1;
    *active_refine = Some(ActiveRefineContext {
        source_id,
        leading_pause_ms,
        committed_clause_count: 0,
        queued_clause_count: 0,
        source_audio: Vec::new(),
        pending_clause_leading_pause_ms: 0,
        dormant: false,
    });
    active_refine
        .as_mut()
        .expect("active refine context must exist")
}

fn ensure_active_refine_context_for_partial(
    active_refine: &mut Option<ActiveRefineContext>,
    next_refine_source_id: &mut u64,
    leading_pause_ms: u32,
    sentence_pause_ms: u32,
    partial_text: &str,
    segments: &mut [TranscriptSegment],
) -> bool {
    if matches!(active_refine.as_ref(), Some(context) if !context.dormant) {
        return false;
    }

    let should_reopen = active_refine
        .as_ref()
        .map(|context| {
            should_reopen_dormant_source(
                context,
                leading_pause_ms,
                sentence_pause_ms,
                partial_text,
                segments,
            )
        })
        .unwrap_or(false);

    if should_reopen {
        let source_id = active_refine
            .as_ref()
            .expect("dormant refine context must exist")
            .source_id;
        reopen_source_tail(segments, source_id);
        let context = active_refine
            .as_mut()
            .expect("dormant refine context must exist");
        context.pending_clause_leading_pause_ms = leading_pause_ms;
        context.dormant = false;
        return true;
    }

    *active_refine = None;
    let _ = activate_refine_context(
        active_refine,
        next_refine_source_id,
        leading_pause_ms,
        sentence_pause_ms,
        segments,
    );
    true
}

fn should_reopen_dormant_source(
    context: &ActiveRefineContext,
    leading_pause_ms: u32,
    sentence_pause_ms: u32,
    partial_text: &str,
    segments: &[TranscriptSegment],
) -> bool {
    if leading_pause_ms >= sentence_pause_ms {
        return false;
    }

    let source_text = source_text_for_matching(segments, context.source_id);
    if source_text.is_empty() || partial_text.is_empty() {
        return false;
    }

    if shared_prefix_chars(&source_text, partial_text) >= 2 {
        return false;
    }

    if source_ends_with_terminal_punctuation(segments, context.source_id) {
        return starts_with_strong_continuation(partial_text);
    }

    true
}

fn source_text_for_matching(segments: &[TranscriptSegment], source_id: u64) -> String {
    let mut text = String::new();
    for segment in segments
        .iter()
        .filter(|segment| segment.source_id == Some(source_id))
    {
        let segment_text = segment
            .corrected_text
            .as_deref()
            .unwrap_or(segment.live_text.as_str());
        text.push_str(&strip_terminal_boundary_punctuation(segment_text));
    }
    text
}

fn source_ends_with_terminal_punctuation(segments: &[TranscriptSegment], source_id: u64) -> bool {
    segments
        .iter()
        .rev()
        .find(|segment| segment.source_id == Some(source_id))
        .and_then(|segment| {
            segment
                .corrected_text
                .as_deref()
                .or(Some(segment.live_text.as_str()))
        })
        .and_then(|text| text.chars().last())
        .is_some_and(|ch| matches!(ch, '。' | '！' | '？' | '.' | '!' | '?'))
}

fn starts_with_strong_continuation(text: &str) -> bool {
    STRONG_CONTINUATION_MARKERS
        .iter()
        .any(|marker| text.starts_with(marker) || marker.starts_with(text))
}

fn shared_prefix_chars(left: &str, right: &str) -> usize {
    left.chars()
        .zip(right.chars())
        .take_while(|(left, right)| left == right)
        .count()
}

fn reopen_source_tail(segments: &mut [TranscriptSegment], source_id: u64) {
    if let Some(segment) = segments
        .iter_mut()
        .rev()
        .find(|segment| segment.source_id == Some(source_id))
    {
        segment.live_text = strip_terminal_boundary_punctuation(&segment.live_text);
        if let Some(corrected_text) = segment.corrected_text.take() {
            let corrected_text = strip_terminal_boundary_punctuation(&corrected_text);
            if !corrected_text.is_empty() {
                segment.corrected_text = Some(corrected_text);
            }
        }
    }
}

fn compose_source_fallback_text(
    segments: &[TranscriptSegment],
    source_id: u64,
    current_partial: &str,
) -> String {
    let mut prefix = String::new();
    for segment in segments
        .iter()
        .filter(|segment| segment.source_id == Some(source_id))
    {
        let text = segment
            .corrected_text
            .as_deref()
            .unwrap_or(segment.live_text.as_str());
        prefix.push_str(&strip_terminal_boundary_punctuation(text));
    }

    if current_partial.is_empty() {
        return prefix;
    }

    if prefix.is_empty() || current_partial.starts_with(&prefix) {
        return current_partial.to_string();
    }

    let overlap_chars = prefix
        .chars()
        .zip(current_partial.chars())
        .take_while(|(left, right)| left == right)
        .count();
    if overlap_chars > 0 {
        let suffix = current_partial
            .chars()
            .skip(overlap_chars)
            .collect::<String>();
        return format!("{}{}", prefix, suffix);
    }

    format!("{}{}", prefix, current_partial)
}

fn push_transcript_segment(
    segments: &mut Vec<TranscriptSegment>,
    next_segment_id: &mut u64,
    source_id: Option<u64>,
    source_clause_index: Option<usize>,
    leading_pause_ms: u32,
    live_text: String,
    corrected_text: Option<String>,
) {
    let segment_id = *next_segment_id;
    *next_segment_id += 1;
    segments.push(TranscriptSegment {
        id: segment_id,
        source_id,
        source_clause_index,
        leading_pause_ms,
        live_text: live_text.clone(),
        corrected_text,
    });
    log::info!(
        "Queued transcript segment: id={}, source_id={:?}, clause_index={:?}, text='{}', corrected={}",
        segment_id,
        source_id,
        source_clause_index,
        live_text,
        segments
            .last()
            .and_then(|segment| segment.corrected_text.as_ref())
            .is_some()
    );
}

fn push_live_clauses_from_display(
    segments: &mut Vec<TranscriptSegment>,
    next_segment_id: &mut u64,
    active_refine: &mut ActiveRefineContext,
    display: &str,
) {
    let (clauses, _) = split_stable_clauses(display);
    let first_new_clause_index = active_refine.committed_clause_count;
    for (index, clause) in clauses
        .into_iter()
        .enumerate()
        .skip(active_refine.committed_clause_count)
    {
        push_transcript_segment(
            segments,
            next_segment_id,
            Some(active_refine.source_id),
            Some(index),
            if index == 0 {
                active_refine.leading_pause_ms
            } else if index == first_new_clause_index {
                let pause_ms = active_refine.pending_clause_leading_pause_ms;
                active_refine.pending_clause_leading_pause_ms = 0;
                pause_ms
            } else {
                0
            },
            clause,
            None,
        );
    }
    active_refine.committed_clause_count =
        clauses_len(display).max(active_refine.committed_clause_count);
}

fn clauses_len(display: &str) -> usize {
    split_stable_clauses(display).0.len()
}

fn queue_refine_task(
    worker: &mut RefineWorker,
    segments: &[TranscriptSegment],
    source_id: u64,
    audio: Vec<f32>,
    fallback_text: String,
    clause_count: usize,
    is_final: bool,
) {
    let audio_stats = analyze_refine_audio(&audio);
    let source_clause_pauses = summarize_source_clause_pauses(segments, source_id);
    log::info!(
        "Queued refine task: source_id={}, audio_samples={}, audio_ms={}, clause_count={}, final={}, source_clause_pauses='{}', low_energy_frames={}, low_energy_runs={}, longest_low_energy_ms={}, longest_internal_low_energy_ms={}, trailing_low_energy_ms={}, fallback='{}'",
        source_id,
        audio.len(),
        audio_stats.duration_ms,
        clause_count,
        is_final,
        source_clause_pauses,
        audio_stats.low_energy_frame_count,
        audio_stats.low_energy_run_count,
        audio_stats.longest_low_energy_ms,
        audio_stats.longest_internal_low_energy_ms,
        audio_stats.trailing_low_energy_ms,
        fallback_text
    );
    let _ = worker.task_tx.send(RefineTask {
        source_id,
        audio,
        fallback_text,
        clause_count,
        is_final,
    });
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

fn process_live_chunk_with_incremental_refine(
    text_config: &TextProcessingConfig,
    live_engine: &mut AsrEngine,
    samples: &[f32],
    current_segment_audio: &mut Vec<f32>,
    segment_overlap_audio: &mut Vec<f32>,
    current_partial: &mut String,
    current_partial_leading_pause_ms: u32,
    active_refine: &mut Option<ActiveRefineContext>,
    next_refine_source_id: &mut u64,
    segments: &mut Vec<TranscriptSegment>,
    next_segment_id: &mut u64,
    refine_worker: &mut RefineWorker,
) -> Result<(), String> {
    if samples.is_empty() {
        return Ok(());
    }

    if let Some(active) = active_refine.as_mut().filter(|active| !active.dormant) {
        active.source_audio.extend_from_slice(samples);
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
            let (tail, tail_leading_pause_ms) = {
                let activated_now = ensure_active_refine_context_for_partial(
                    active_refine,
                    next_refine_source_id,
                    current_partial_leading_pause_ms,
                    text_config.sentence_pause_ms,
                    current_partial,
                    segments.as_mut_slice(),
                );
                let active = active_refine
                    .as_mut()
                    .expect("active refine context must exist after activation");
                if activated_now {
                    active.source_audio.extend_from_slice(current_segment_audio);
                }
                let source_clauses = source_clause_contexts(segments, active.source_id);
                let display = build_refine_display_with_source_context(
                    text_config,
                    &source_clauses,
                    active.committed_clause_count,
                    current_partial,
                    false,
                );
                push_live_clauses_from_display(segments, next_segment_id, active, &display);
                let clause_count = split_stable_clauses(&display).0.len();
                if clause_count > active.queued_clause_count
                    && active.source_audio.len() >= MIN_REFINE_SEGMENT_SAMPLES
                {
                    queue_refine_task(
                        refine_worker,
                        segments,
                        active.source_id,
                        active.source_audio.clone(),
                        compose_source_fallback_text(segments, active.source_id, current_partial),
                        clause_count,
                        false,
                    );
                    active.queued_clause_count = clause_count;
                }
                (
                    remaining_display_after_clause_count(&display, active.committed_clause_count),
                    tail_leading_pause_ms(Some(active)),
                )
            };

            sync_runtime_transcript(text_config, segments, None, &tail, tail_leading_pause_ms);
            state::set_state(AppState::Transcribing);
            state::set_status_message("Live ASR is producing partial text");
            log::info!(
                "Live ASR incremental partial updated: samples={}, full='{}', tail='{}'",
                samples.len(),
                current_partial,
                tail
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

fn finalize_segment_with_incremental_refine(
    text_config: &TextProcessingConfig,
    live_engine: &mut AsrEngine,
    current_segment_audio: &mut Vec<f32>,
    segment_overlap_audio: &mut Vec<f32>,
    current_segment_leading_pause_ms: &mut u32,
    active_refine: &mut Option<ActiveRefineContext>,
    next_refine_source_id: &mut u64,
    current_partial: &mut String,
    segments: &mut Vec<TranscriptSegment>,
    next_segment_id: &mut u64,
    refine_worker: &mut RefineWorker,
) -> Result<(), String> {
    if current_segment_audio.is_empty() && current_partial.is_empty() {
        return Ok(());
    }

    let live_text = live_engine.finalize().map_err(|e| e.to_string())?;
    let live_text = if live_text.is_empty() {
        current_partial.clone()
    } else {
        live_text
    };
    current_partial.clear();

    if live_text.is_empty() {
        current_segment_audio.clear();
        *current_segment_leading_pause_ms = 0;
        *active_refine = None;
        sync_runtime_transcript(text_config, segments, None, "", 0);
        return Ok(());
    }

    let overlap_len = current_segment_audio.len().min(SEGMENT_OVERLAP_SAMPLES);
    *segment_overlap_audio =
        current_segment_audio[current_segment_audio.len() - overlap_len..].to_vec();

    let final_display;
    let clause_count;
    {
        let activated_now = ensure_active_refine_context_for_partial(
            active_refine,
            next_refine_source_id,
            *current_segment_leading_pause_ms,
            text_config.sentence_pause_ms,
            &live_text,
            segments.as_mut_slice(),
        );
        let active = active_refine
            .as_mut()
            .expect("active refine context must exist after finalize activation");
        if activated_now {
            active.source_audio.extend_from_slice(current_segment_audio);
        }
        let source_clauses = source_clause_contexts(segments, active.source_id);
        final_display = build_refine_display_with_source_context(
            text_config,
            &source_clauses,
            active.committed_clause_count,
            &live_text,
            true,
        );
        clause_count = split_stable_clauses(&final_display).0.len();
        push_live_clauses_from_display(segments, next_segment_id, active, &final_display);
        if active.source_audio.len() >= MIN_REFINE_SEGMENT_SAMPLES {
            queue_refine_task(
                refine_worker,
                segments,
                active.source_id,
                active.source_audio.clone(),
                compose_source_fallback_text(segments, active.source_id, ""),
                clause_count,
                true,
            );
            active.queued_clause_count = active.queued_clause_count.max(clause_count);
        }
        active.dormant = true;
        active.pending_clause_leading_pause_ms = 0;
    }

    current_segment_audio.clear();
    *current_segment_leading_pause_ms = 0;
    sync_runtime_transcript(text_config, segments, None, "", 0);
    Ok(())
}

fn recover_live_segment_with_incremental_refine(
    text_config: &TextProcessingConfig,
    live_engine: &mut AsrEngine,
    current_segment_audio: &mut Vec<f32>,
    segment_overlap_audio: &mut Vec<f32>,
    current_segment_leading_pause_ms: &mut u32,
    active_refine: &mut Option<ActiveRefineContext>,
    next_refine_source_id: &mut u64,
    current_partial: &mut String,
    segments: &mut Vec<TranscriptSegment>,
    next_segment_id: &mut u64,
    err: &str,
) {
    log::warn!("Live ASR chunk failed, resetting active segment: {}", err);
    let fallback_text = current_partial.trim().to_string();

    live_engine.reset();
    current_segment_audio.clear();
    segment_overlap_audio.clear();
    current_partial.clear();

    if !fallback_text.is_empty() {
        let _ = ensure_active_refine_context_for_partial(
            active_refine,
            next_refine_source_id,
            *current_segment_leading_pause_ms,
            text_config.sentence_pause_ms,
            &fallback_text,
            segments.as_mut_slice(),
        );
        let active = active_refine
            .as_mut()
            .expect("active refine context must exist after recovery activation");
        let source_clauses = source_clause_contexts(segments, active.source_id);
        let final_display = build_refine_display_with_source_context(
            text_config,
            &source_clauses,
            active.committed_clause_count,
            &fallback_text,
            true,
        );
        push_live_clauses_from_display(segments, next_segment_id, active, &final_display);
    }

    *current_segment_leading_pause_ms = 0;
    *active_refine = None;
    sync_runtime_transcript(text_config, segments, None, "", 0);
    state::set_state(AppState::Recording);
    state::set_status_message(format!("实时转写分段异常，已自动恢复: {}", err));
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
    _refine_worker: Option<&mut RefineWorker>,
    leading_pause_ms: u32,
    segment_audio: Vec<f32>,
    live_text: String,
) {
    let segment_id = *next_segment_id;
    *next_segment_id += 1;
    segments.push(TranscriptSegment {
        id: segment_id,
        source_id: None,
        source_clause_index: None,
        leading_pause_ms,
        live_text: live_text.clone(),
        corrected_text: None,
    });
    log::info!(
        "Queued transcript segment: id={}, audio_samples={}, text='{}', refine_worker={}",
        segment_id,
        segment_audio.len(),
        live_text,
        false
    );
}

fn collect_refine_results(
    text_config: &TextProcessingConfig,
    result_rx: &mpsc::Receiver<RefineResult>,
    segments: &mut [TranscriptSegment],
) -> bool {
    let mut changed = false;
    while let Ok(result) = result_rx.try_recv() {
        changed |= apply_refine_result(text_config, segments, result);
    }
    changed
}

fn collect_refine_results_blocking(
    text_config: &TextProcessingConfig,
    result_rx: &mpsc::Receiver<RefineResult>,
    segments: &mut [TranscriptSegment],
) {
    while let Ok(result) = result_rx.recv_timeout(Duration::from_millis(20)) {
        let _ = apply_refine_result(text_config, segments, result);
    }
}

fn apply_refine_result(
    text_config: &TextProcessingConfig,
    segments: &mut [TranscriptSegment],
    result: RefineResult,
) -> bool {
    let source_clauses = source_clause_contexts(segments, result.source_id);
    let rendered = build_refine_display_with_source_context(
        text_config,
        &source_clauses,
        result.clause_count.saturating_sub(1),
        &result.text,
        result.is_final,
    );
    let (mut clauses, tail) = split_stable_clauses(&rendered);
    if result.is_final && !tail.is_empty() {
        clauses.push(tail);
    }
    if clauses.is_empty() && !rendered.is_empty() {
        clauses.push(rendered);
    }

    let apply_count = result.clause_count.min(clauses.len());
    log::info!(
        "Refine result received: source_id={}, final={}, requested_clause_count={}, parsed_clause_count={}, rendered='{}'",
        result.source_id,
        result.is_final,
        result.clause_count,
        clauses.len(),
        clauses.join("")
    );
    let mut applied = 0usize;
    for segment in segments.iter_mut().filter(|segment| {
        segment.source_id == Some(result.source_id) && segment.source_clause_index.is_some()
    }) {
        let Some(index) = segment.source_clause_index else {
            continue;
        };
        if index >= apply_count {
            continue;
        }

        let corrected = clauses[index].clone();
        let before = segment
            .corrected_text
            .as_deref()
            .unwrap_or(segment.live_text.as_str())
            .to_string();
        if segment.corrected_text.as_deref() != Some(corrected.as_str()) {
            segment.corrected_text = Some(corrected);
            applied += 1;
            log::info!(
                "Refine clause updated: source_id={}, clause_index={}, segment_id={}, before='{}', after='{}'",
                result.source_id,
                index,
                segment.id,
                before,
                segment.corrected_text.as_deref().unwrap_or("")
            );
        } else {
            log::info!(
                "Refine clause unchanged: source_id={}, clause_index={}, segment_id={}, text='{}'",
                result.source_id,
                index,
                segment.id,
                before
            );
        }
    }

    log::info!(
        "Refine result applied: source_id={}, clauses_applied={}, clause_count={}, final={}",
        result.source_id,
        applied,
        apply_count,
        result.is_final
    );
    if applied == 0 && apply_count == 0 {
        log::debug!(
            "Refine result had no stable clauses to apply: source_id={}, text='{}'",
            result.source_id,
            result.text
        );
    }
    applied > 0
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
    use super::{
        activate_refine_context, analyze_refine_audio, apply_refine_result,
        build_refine_display_with_source_context, compose_source_fallback_text,
        effective_segment_leading_pause_ms, ensure_active_refine_context_for_partial,
        remaining_display_after_clause_count, remaining_text_after_prefix, source_clause_contexts,
        split_stable_clauses, ActiveRefineContext, RefineAudioStats, RefineResult,
        TextProcessingConfig, TranscriptSegment,
    };

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

    #[test]
    fn split_stable_clauses_separates_tail() {
        let (clauses, tail) = split_stable_clauses("中文流式语音识别模型，适合低延迟实时转");
        assert_eq!(clauses, vec!["中文流式语音识别模型，"]);
        assert_eq!(tail, "适合低延迟实时转");
    }

    #[test]
    fn remaining_display_skips_committed_clauses() {
        let tail =
            remaining_display_after_clause_count("中文流式语音识别模型，适合低延迟实时转", 1);
        assert_eq!(tail, "适合低延迟实时转");
    }

    #[test]
    fn refine_result_updates_matching_source_clauses() {
        let text_config = TextProcessingConfig::default();
        let mut segments = vec![
            TranscriptSegment {
                id: 1,
                source_id: Some(7),
                source_clause_index: Some(0),
                leading_pause_ms: 0,
                live_text: "中文流式语音识别模型，".to_string(),
                corrected_text: None,
            },
            TranscriptSegment {
                id: 2,
                source_id: Some(7),
                source_clause_index: Some(1),
                leading_pause_ms: 0,
                live_text: "适合低延迟实时转写。".to_string(),
                corrected_text: None,
            },
            TranscriptSegment {
                id: 3,
                source_id: Some(8),
                source_clause_index: Some(0),
                leading_pause_ms: 0,
                live_text: "不相关。".to_string(),
                corrected_text: None,
            },
        ];

        apply_refine_result(
            &text_config,
            &mut segments,
            RefineResult {
                source_id: 7,
                text: "中文流式语音识别模型适合低延迟实时转写".to_string(),
                clause_count: 2,
                is_final: true,
            },
        );

        assert_eq!(
            segments[0].corrected_text.as_deref(),
            Some("中文流式语音识别模型，")
        );
        assert_eq!(
            segments[1].corrected_text.as_deref(),
            Some("适合低延迟实时转写。")
        );
        assert_eq!(segments[2].corrected_text, None);
    }

    #[test]
    fn compose_source_fallback_text_does_not_duplicate_committed_prefix() {
        let segments = vec![TranscriptSegment {
            id: 1,
            source_id: Some(7),
            source_clause_index: Some(0),
            leading_pause_ms: 0,
            live_text: "中文流式语音识别模型，".to_string(),
            corrected_text: None,
        }];

        let result =
            compose_source_fallback_text(&segments, 7, "中文流式语音识别模型适合低延迟实时转写");

        assert_eq!(result, "中文流式语音识别模型适合低延迟实时转写");
    }

    #[test]
    fn remaining_text_after_prefix_strips_committed_source_prefix() {
        let result = remaining_text_after_prefix(
            "中文流式语音识别模型",
            "中文流式语音识别模型适合低延迟实时转写",
        );

        assert_eq!(result, "适合低延迟实时转写");
    }

    #[test]
    fn refine_display_with_source_context_preserves_committed_boundaries() {
        let text_config = TextProcessingConfig::default();
        let segments = vec![TranscriptSegment {
            id: 1,
            source_id: Some(7),
            source_clause_index: Some(0),
            leading_pause_ms: 0,
            live_text: "中文流式语音识别模型，".to_string(),
            corrected_text: None,
        }];

        let source_clauses = source_clause_contexts(&segments, 7);
        let rendered = build_refine_display_with_source_context(
            &text_config,
            &source_clauses,
            1,
            "中文流式语音识别模型适合低延迟实时转写",
            false,
        );

        assert_eq!(rendered, "中文流式语音识别模型，适合低延迟实时转写");
    }

    #[test]
    fn analyze_refine_audio_reports_internal_and_trailing_low_energy_spans() {
        let mut audio = Vec::new();
        audio.extend(vec![0.1f32; 512 * 2]);
        audio.extend(vec![0.0f32; 512 * 2]);
        audio.extend(vec![0.1f32; 512]);
        audio.extend(vec![0.0f32; 512 * 3]);

        let stats = analyze_refine_audio(&audio);

        assert_eq!(
            stats,
            RefineAudioStats {
                duration_ms: 256,
                low_energy_frame_count: 5,
                low_energy_run_count: 2,
                longest_low_energy_ms: 96,
                longest_internal_low_energy_ms: 64,
                trailing_low_energy_ms: 96,
            }
        );
    }

    #[test]
    fn activate_refine_context_does_not_reopen_dormant_source_implicitly() {
        let mut segments = vec![TranscriptSegment {
            id: 1,
            source_id: Some(1),
            source_clause_index: Some(0),
            leading_pause_ms: 0,
            live_text: "中文流式语音识别模型。".to_string(),
            corrected_text: None,
        }];
        let mut active_refine = Some(ActiveRefineContext {
            source_id: 1,
            leading_pause_ms: 0,
            committed_clause_count: 1,
            queued_clause_count: 1,
            source_audio: Vec::new(),
            pending_clause_leading_pause_ms: 0,
            dormant: true,
        });
        let mut next_refine_source_id = 2;

        let active = activate_refine_context(
            &mut active_refine,
            &mut next_refine_source_id,
            1500,
            2800,
            segments.as_mut_slice(),
        );

        assert_eq!(active.source_id, 2);
        assert_eq!(segments[0].live_text, "中文流式语音识别模型。");
    }

    #[test]
    fn repeated_sentence_after_terminal_punctuation_starts_new_source() {
        let mut segments = vec![
            TranscriptSegment {
                id: 1,
                source_id: Some(1),
                source_clause_index: Some(0),
                leading_pause_ms: 0,
                live_text: "中文流式语音识别模型，".to_string(),
                corrected_text: None,
            },
            TranscriptSegment {
                id: 2,
                source_id: Some(1),
                source_clause_index: Some(1),
                leading_pause_ms: 0,
                live_text: "适合低延迟实时转写。".to_string(),
                corrected_text: None,
            },
        ];
        let mut active_refine = Some(ActiveRefineContext {
            source_id: 1,
            leading_pause_ms: 0,
            committed_clause_count: 2,
            queued_clause_count: 2,
            source_audio: Vec::new(),
            pending_clause_leading_pause_ms: 0,
            dormant: true,
        });
        let mut next_refine_source_id = 2;

        let activated_now = ensure_active_refine_context_for_partial(
            &mut active_refine,
            &mut next_refine_source_id,
            1500,
            2800,
            "中文",
            segments.as_mut_slice(),
        );

        assert!(activated_now);
        let active = active_refine.expect("new source should be created");
        assert_eq!(active.source_id, 2);
        assert!(!active.dormant);
        assert_eq!(next_refine_source_id, 3);
        assert_eq!(segments[1].live_text, "适合低延迟实时转写。");
    }

    #[test]
    fn short_continuation_after_comma_reopens_existing_source() {
        let mut segments = vec![TranscriptSegment {
            id: 1,
            source_id: Some(1),
            source_clause_index: Some(0),
            leading_pause_ms: 0,
            live_text: "中文流式语音识别模型，".to_string(),
            corrected_text: None,
        }];
        let mut active_refine = Some(ActiveRefineContext {
            source_id: 1,
            leading_pause_ms: 0,
            committed_clause_count: 1,
            queued_clause_count: 1,
            source_audio: Vec::new(),
            pending_clause_leading_pause_ms: 0,
            dormant: true,
        });
        let mut next_refine_source_id = 2;

        let activated_now = ensure_active_refine_context_for_partial(
            &mut active_refine,
            &mut next_refine_source_id,
            1500,
            2800,
            "适",
            segments.as_mut_slice(),
        );

        assert!(activated_now);
        let active = active_refine.expect("existing source should be reopened");
        assert_eq!(active.source_id, 1);
        assert!(!active.dormant);
        assert_eq!(next_refine_source_id, 2);
        assert_eq!(segments[0].live_text, "中文流式语音识别模型");
    }
}

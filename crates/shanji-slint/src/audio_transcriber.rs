use shanji_core::asr::{AsrConfig, AsrEngine};
use shanji_core::audio::{self, AudioCapture};
use shanji_core::config::{self, AppConfig, AppState};
use shanji_core::history::{HistoryDb, HistoryRecord};
use shanji_core::hotwords;
use shanji_core::llm;
use shanji_core::model;
use shanji_core::offline_transcribe::OfflineTranscriber;
use shanji_core::output;
use shanji_core::paths::AppPaths;
use shanji_core::state;
use shanji_core::text_processing::{
    finalize_existing_punctuation_text, finalize_transcript_text,
    normalize_existing_punctuation_text, render_segmented_transcript, TextProcessingConfig,
    TranscriptChunk,
};
use shanji_core::vad::{self, VadDetector, VadEvent};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingPauseBoundary {
    pause_ms: u32,
    rough_split_chars: usize,
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

const PAUSE_BOUNDARY_RIGHT_MARKERS: &[&str] = &[
    "不", "没", "无", "并", "而", "但", "还", "也", "就", "又", "都", "要", "会", "能", "可",
    "可以", "能够", "支持", "适合", "用于", "隐", "隐私", "我们", "你", "我", "他", "她", "它",
    "这", "那",
];

const PAUSE_BOUNDARY_MAX_SHIFT_CHARS: usize = 3;

const SEGMENT_OVERLAP_SAMPLES: usize = 3_200;
const MIN_REFINE_SEGMENT_SAMPLES: usize = 16_000;
const REFINE_AUDIO_ANALYSIS_FRAME_SAMPLES: usize = 512;
const REFINE_AUDIO_LOW_ENERGY_RMS_THRESHOLD: f32 = 0.01;

static PENDING_PASTE: OnceLock<Mutex<Option<PendingPaste>>> = OnceLock::new();

fn pending_paste_slot() -> &'static Mutex<Option<PendingPaste>> {
    PENDING_PASTE.get_or_init(|| Mutex::new(None))
}

struct PreloadedLiveEngine {
    engine: AsrEngine,
    model_id: String,
}

struct PreloadedSegmentTranscriber {
    transcriber: OfflineTranscriber,
    model_id: String,
}

static PRELOADED_LIVE: OnceLock<Mutex<Option<PreloadedLiveEngine>>> = OnceLock::new();
static PRELOADED_SEGMENT: OnceLock<Mutex<Option<PreloadedSegmentTranscriber>>> = OnceLock::new();

fn preloaded_live_slot() -> &'static Mutex<Option<PreloadedLiveEngine>> {
    PRELOADED_LIVE.get_or_init(|| Mutex::new(None))
}

fn preloaded_segment_slot() -> &'static Mutex<Option<PreloadedSegmentTranscriber>> {
    PRELOADED_SEGMENT.get_or_init(|| Mutex::new(None))
}

pub fn preload(paths: AppPaths) {
    preload_live(paths.clone());
    preload_refine(paths);
}

pub fn preload_live(paths: AppPaths) {
    std::thread::spawn(move || {
        let config = match config::get_config(&paths) {
            Ok(c) => c,
            Err(_) => return,
        };
        if !model::is_model_downloaded_with_paths(&paths, &config.asr.live_model_id) {
            return;
        }
        {
            let slot = preloaded_live_slot().lock().unwrap();
            if slot
                .as_ref()
                .map(|c| c.model_id == config.asr.live_model_id)
                .unwrap_or(false)
            {
                return;
            }
        }
        match build_engine(&paths, &config, &[], &config.asr.live_model_id) {
            Ok(engine) => {
                let mut slot = preloaded_live_slot().lock().unwrap();
                *slot = Some(PreloadedLiveEngine {
                    engine,
                    model_id: config.asr.live_model_id.clone(),
                });
                log::info!("Preloaded live ASR engine: {}", config.asr.live_model_id);
            }
            Err(e) => log::warn!("Preload live engine failed: {}", e),
        }
    });
}

pub fn preload_refine(paths: AppPaths) {
    std::thread::spawn(move || {
        let config = match config::get_config(&paths) {
            Ok(c) => c,
            Err(_) => return,
        };
        if !config.asr.refine_enabled {
            return;
        }
        if !model::is_model_downloaded_with_paths(&paths, &config.asr.refine_model_id) {
            return;
        }
        {
            let slot = preloaded_segment_slot().lock().unwrap();
            if slot
                .as_ref()
                .map(|c| c.model_id == config.asr.refine_model_id)
                .unwrap_or(false)
            {
                return;
            }
        }
        let text_config = TextProcessingConfig {
            punct_style: config.asr.punct_style.clone(),
            insert_punct: config.asr.insert_punct,
            comma_pause_ms: config.asr.comma_pause_ms,
            sentence_pause_ms: config.asr.sentence_pause_ms,
            hotwords: Vec::new(),
        };
        match OfflineTranscriber::new(&paths, &config.asr.refine_model_id, text_config) {
            Ok(transcriber) => {
                let mut slot = preloaded_segment_slot().lock().unwrap();
                *slot = Some(PreloadedSegmentTranscriber {
                    transcriber,
                    model_id: config.asr.refine_model_id.clone(),
                });
                log::info!(
                    "Preloaded segment transcriber: {}",
                    config.asr.refine_model_id
                );
            }
            Err(e) => log::warn!("Preload segment transcriber failed: {}", e),
        }
    });
}

pub fn unload_refine() {
    if let Ok(mut slot) = preloaded_segment_slot().lock() {
        *slot = None;
        log::info!("Unloaded preloaded segment transcriber");
    }
}

static LIVE_ASR: OnceLock<Mutex<Option<LiveAsrSession>>> = OnceLock::new();

fn live_asr_slot() -> &'static Mutex<Option<LiveAsrSession>> {
    LIVE_ASR.get_or_init(|| Mutex::new(None))
}

static LIVE_ASR_STOPPING: OnceLock<AtomicBool> = OnceLock::new();

fn live_asr_stopping_flag() -> &'static AtomicBool {
    LIVE_ASR_STOPPING.get_or_init(|| AtomicBool::new(false))
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
    let Some(session) = take_live_asr_session()? else {
        if is_stopping() {
            while is_stopping() {
                std::thread::sleep(Duration::from_millis(10));
            }
            return Ok(true);
        }
        return Ok(false);
    };

    live_asr_stopping_flag().store(true, Ordering::SeqCst);
    finalize_live_asr_stop(session);
    live_asr_stopping_flag().store(false, Ordering::SeqCst);
    Ok(true)
}

pub fn is_running() -> bool {
    if is_stopping() {
        return true;
    }

    live_asr_slot()
        .lock()
        .map(|slot| slot.is_some())
        .unwrap_or(false)
}

pub fn is_stopping() -> bool {
    live_asr_stopping_flag().load(Ordering::SeqCst)
}

pub fn request_stop() -> Result<bool, String> {
    let Some(session) = take_live_asr_session()? else {
        return Ok(false);
    };

    live_asr_stopping_flag().store(true, Ordering::SeqCst);
    std::thread::spawn(move || {
        finalize_live_asr_stop(session);
        live_asr_stopping_flag().store(false, Ordering::SeqCst);
    });
    Ok(true)
}

fn take_live_asr_session() -> Result<Option<LiveAsrSession>, String> {
    let mut slot = live_asr_slot()
        .lock()
        .map_err(|_| "Live ASR lock poisoned".to_string())?;
    Ok(slot.take())
}

fn finalize_live_asr_stop(session: LiveAsrSession) {
    log::info!("Stopping live ASR session");
    let _ = session.stop_tx.send(());
    let _ = session.join_handle.join();
    complete_pending_paste();
}

fn complete_pending_paste() {
    let Some(paste) = pending_paste_slot()
        .lock()
        .ok()
        .and_then(|mut pending| pending.take())
    else {
        return;
    };

    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));

        let queue_result = slint::invoke_from_event_loop(move || {
            let pasted = match output::simulate_paste() {
                Ok(()) => true,
                Err(err) => {
                    state::set_status_message(format!(
                        "Auto-paste unavailable, text is in clipboard: {}",
                        err
                    ));
                    false
                }
            };

            if pasted {
                if let Some(backup) = paste.clipboard_backup {
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(440));
                        let _ = output::copy_to_clipboard(&backup);
                    });
                }
            }
        });

        if let Err(err) = queue_result {
            log::warn!("Failed to queue pending paste on UI thread: {}", err);
            state::set_status_message(format!(
                "Auto-paste unavailable, text is in clipboard: {}",
                err
            ));
        }
    });
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

    let mut live_engine = {
        let mut slot = preloaded_live_slot().lock().unwrap();
        match slot
            .take()
            .filter(|c| c.model_id == config.asr.live_model_id)
        {
            Some(mut cached) => {
                cached.engine.reset();
                cached.engine.reconfigure(AsrConfig {
                    insert_punct: config.asr.insert_punct,
                    punct_style: config.asr.punct_style.clone(),
                    hotwords: text_config.hotwords.clone(),
                    ..AsrConfig::default()
                });
                log::info!(
                    "Using preloaded live ASR engine: {}",
                    config.asr.live_model_id
                );
                cached.engine
            }
            None => build_engine(
                &paths,
                &config,
                &text_config.hotwords,
                &config.asr.live_model_id,
            )?,
        }
    };
    let mut live_segment_transcriber = if config.asr.refine_enabled {
        let mut slot = preloaded_segment_slot().lock().unwrap();
        match slot
            .take()
            .filter(|c| c.model_id == config.asr.refine_model_id)
        {
            Some(mut cached) => {
                cached.transcriber.update_text_config(text_config.clone());
                log::info!(
                    "Using preloaded segment transcriber: {}",
                    config.asr.refine_model_id
                );
                Some(cached.transcriber)
            }
            None => build_live_segment_transcriber(&paths, &config, &text_config.hotwords),
        }
    } else {
        None
    };
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
    let mut pending_pause_boundary: Option<PendingPauseBoundary> = None;
    let mut current_partial = String::new();
    let mut segments: Vec<TranscriptSegment> = Vec::new();
    let mut next_segment_id = 1u64;
    let mut next_refine_source_id = 1u64;

    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }

        if let Some(worker) = refine_worker.as_mut() {
            if collect_refine_results(
                &text_config,
                &worker.result_rx,
                &mut segments,
                &mut next_segment_id,
            ) {
                let tail = if current_partial.is_empty() {
                    String::new()
                } else if let Some(active) = active_refine.as_ref() {
                    let source_clauses = source_clause_contexts(&segments, active.source_id);
                    let source_match_clauses =
                        source_live_clause_contexts(&segments, active.source_id);
                    let display = build_refine_display_with_source_context(
                        &text_config,
                        &source_clauses,
                        &source_match_clauses,
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
                                    &mut pending_pause_boundary,
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
                                        &mut pending_pause_boundary,
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
                            VadEvent::Speech {
                                samples: chunk,
                                leading_pause_ms,
                            } => {
                                log::debug!(
                                    "VAD speech event: samples={}, leading_pause_ms={}",
                                    chunk.len(),
                                    leading_pause_ms
                                );
                                if current_segment_audio.is_empty() {
                                    current_segment_leading_pause_ms =
                                        effective_segment_leading_pause_ms(
                                            accumulated_silence_ms,
                                            pending_vad_boundary_pause_ms,
                                        );
                                    pending_vad_boundary_pause_ms = 0;
                                }
                                if refine_worker.is_some() {
                                    arm_pending_vad_pause_boundary(
                                        &text_config,
                                        &current_partial,
                                        &mut pending_pause_boundary,
                                        leading_pause_ms,
                                    );
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
                                        &mut pending_pause_boundary,
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
                                            &mut pending_pause_boundary,
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
                                        &mut pending_pause_boundary,
                                        &mut next_refine_source_id,
                                        &mut current_partial,
                                        &mut segments,
                                        &mut next_segment_id,
                                        worker,
                                        live_segment_transcriber.as_mut(),
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
                                        live_segment_transcriber.as_mut(),
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
                                            &mut pending_pause_boundary,
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
                            &mut pending_pause_boundary,
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
                                &mut pending_pause_boundary,
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
            &mut pending_pause_boundary,
            &mut next_refine_source_id,
            &mut current_partial,
            &mut segments,
            &mut next_segment_id,
            worker,
            live_segment_transcriber.as_mut(),
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
            live_segment_transcriber.as_mut(),
            true,
        )?;
    }

    if let Some(worker) = refine_worker {
        drop(worker.task_tx);
        let _ = worker.join_handle.join();
        collect_refine_results_blocking(
            &text_config,
            &worker.result_rx,
            &mut segments,
            &mut next_segment_id,
        );
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

    // Return engines to preload cache for next session
    live_engine.reset();
    if let Ok(mut slot) = preloaded_live_slot().lock() {
        if slot.is_none() {
            *slot = Some(PreloadedLiveEngine {
                engine: live_engine,
                model_id: config.asr.live_model_id.clone(),
            });
        }
    }
    if let Some(transcriber) = live_segment_transcriber {
        if let Ok(mut slot) = preloaded_segment_slot().lock() {
            if slot.is_none() {
                *slot = Some(PreloadedSegmentTranscriber {
                    transcriber,
                    model_id: config.asr.refine_model_id.clone(),
                });
            }
        }
    }

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
        hotwords: hotwords.to_vec(),
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

fn build_text_processing_config(
    config: &AppConfig,
    hotwords: &[shanji_core::hotwords::Hotword],
) -> TextProcessingConfig {
    TextProcessingConfig {
        punct_style: config.asr.punct_style.clone(),
        insert_punct: config.asr.insert_punct,
        comma_pause_ms: config.asr.comma_pause_ms,
        sentence_pause_ms: config.asr.sentence_pause_ms,
        hotwords: hotwords.to_vec(),
    }
}

fn build_live_segment_transcriber(
    paths: &AppPaths,
    config: &AppConfig,
    hotwords: &[shanji_core::hotwords::Hotword],
) -> Option<OfflineTranscriber> {
    if !model::is_model_downloaded_with_paths(paths, &config.asr.refine_model_id) {
        return None;
    }

    match OfflineTranscriber::new(
        paths,
        &config.asr.refine_model_id,
        build_text_processing_config(config, hotwords),
    ) {
        Ok(transcriber) => {
            log::info!(
                "Live segment auxiliary transcriber ready: model={}",
                config.asr.refine_model_id
            );
            Some(transcriber)
        }
        Err(err) => {
            log::warn!(
                "Failed to initialize live segment auxiliary transcriber, falling back to raw live finalize: {}",
                err
            );
            None
        }
    }
}

fn effective_segment_leading_pause_ms(accumulated_silence_ms: u32, boundary_pause_ms: u32) -> u32 {
    accumulated_silence_ms.saturating_add(boundary_pause_ms)
}

fn spawn_refine_worker(paths: AppPaths, config: AppConfig) -> Result<RefineWorker, String> {
    let (task_tx, task_rx) = mpsc::channel::<RefineTask>();
    let (result_tx, result_rx) = mpsc::channel::<RefineResult>();
    let join_handle = std::thread::spawn(move || {
        let hotwords = hotwords::load_all_enabled_with_paths(&paths).unwrap_or_default();
        let text_config = build_text_processing_config(&config, &hotwords);
        let mut transcriber =
            match OfflineTranscriber::new(&paths, &config.asr.refine_model_id, text_config) {
                Ok(transcriber) => transcriber,
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
            let refine_output = refine_segment(&mut transcriber, &audio, is_final);
            let (
                result_text,
                raw_output,
                punctuated_output,
                segment_count,
                used_vad,
                used_punc,
                used_fallback,
            ) = match refine_output {
                Ok(result) if !result.final_text.trim().is_empty() => (
                    result.final_text.clone(),
                    result.raw_text,
                    result.punctuated_text.unwrap_or_default(),
                    result.segment_count,
                    result.used_vad,
                    result.used_punc,
                    false,
                ),
                Ok(result) => (
                    fallback_text.clone(),
                    result.raw_text,
                    result.punctuated_text.unwrap_or_default(),
                    result.segment_count,
                    result.used_vad,
                    result.used_punc,
                    true,
                ),
                Err(err) => {
                    log::warn!(
                        "Refine worker failed: source_id={}, final={}, clause_count={}, error={}",
                        source_id,
                        is_final,
                        clause_count,
                        err
                    );
                    (
                        fallback_text.clone(),
                        String::new(),
                        String::new(),
                        0,
                        false,
                        false,
                        true,
                    )
                }
            };
            log::info!(
                "Refine worker output: source_id={}, final={}, clause_count={}, segment_count={}, used_vad={}, used_punc={}, fallback='{}', raw='{}', punctuated='{}', chosen='{}', used_fallback={}",
                source_id,
                is_final,
                clause_count,
                segment_count,
                used_vad,
                used_punc,
                fallback_text,
                raw_output,
                punctuated_output,
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

fn refine_segment(
    transcriber: &mut OfflineTranscriber,
    audio: &[f32],
    is_final: bool,
) -> Result<shanji_core::offline_transcribe::OfflineTranscriptionResult, String> {
    transcriber
        .transcribe(audio, is_final)
        .map_err(|e| e.to_string())
}

fn transcribe_live_final_segment(
    transcriber: Option<&mut OfflineTranscriber>,
    audio: &[f32],
    fallback_text: &str,
) -> String {
    let Some(transcriber) = transcriber else {
        return fallback_text.to_string();
    };

    match refine_segment(transcriber, audio, true) {
        Ok(result) if !result.final_text.trim().is_empty() => {
            log::info!(
                "Live segment auxiliary transcription: audio_samples={}, segment_count={}, used_vad={}, used_punc={}, raw='{}', punctuated='{}', final='{}'",
                audio.len(),
                result.segment_count,
                result.used_vad,
                result.used_punc,
                result.raw_text,
                result.punctuated_text.as_deref().unwrap_or(""),
                result.final_text
            );
            result.final_text
        }
        Ok(result) => {
            log::warn!(
                "Live segment auxiliary transcription returned empty final text, using live fallback: audio_samples={}, raw='{}'",
                audio.len(),
                result.raw_text
            );
            fallback_text.to_string()
        }
        Err(err) => {
            log::warn!(
                "Live segment auxiliary transcription failed, using live fallback: audio_samples={}, error={}",
                audio.len(),
                err
            );
            fallback_text.to_string()
        }
    }
}

fn source_clause_contexts(
    segments: &[TranscriptSegment],
    source_id: u64,
) -> Vec<SourceClauseContext> {
    source_clause_contexts_with_text(segments, source_id, true)
}

fn source_live_clause_contexts(
    segments: &[TranscriptSegment],
    source_id: u64,
) -> Vec<SourceClauseContext> {
    source_clause_contexts_with_text(segments, source_id, false)
}

fn source_clause_contexts_with_text(
    segments: &[TranscriptSegment],
    source_id: u64,
    prefer_corrected: bool,
) -> Vec<SourceClauseContext> {
    let mut clauses = segments
        .iter()
        .filter_map(|segment| {
            (segment.source_id == Some(source_id))
                .then_some(segment.source_clause_index)
                .flatten()
                .map(|index| {
                    let text = if prefer_corrected {
                        segment
                            .corrected_text
                            .clone()
                            .unwrap_or_else(|| segment.live_text.clone())
                    } else {
                        segment.live_text.clone()
                    };
                    (
                        index,
                        SourceClauseContext {
                            text,
                            leading_pause_ms: segment.leading_pause_ms,
                        },
                    )
                })
        })
        .filter(|(_, clause)| is_meaningful_clause_text(&clause.text))
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
    source_match_clauses: &[SourceClauseContext],
    anchor_clause_count: usize,
    text: &str,
    is_final: bool,
) -> String {
    let anchor_clause_count = anchor_clause_count
        .min(source_clauses.len())
        .min(source_match_clauses.len());
    let committed_clauses = &source_clauses[..anchor_clause_count];
    let committed_match_clauses = &source_match_clauses[..anchor_clause_count];
    let committed_prefix = boundaryless_clause_text(committed_match_clauses);
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
        .or_else(|| source_match_clauses.get(anchor_clause_count))
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
            if is_meaningful_clause_text(&clause) {
                clauses.push(clause);
            }
            current.clear();
        }
    }

    let tail = current.trim().to_string();
    let tail = if is_meaningful_clause_text(&tail) {
        tail
    } else {
        String::new()
    };

    (clauses, tail)
}

fn is_meaningful_clause_text(text: &str) -> bool {
    text.chars()
        .any(|ch| !ch.is_whitespace() && !is_boundary_punctuation(ch))
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
        text.push_str(&strip_terminal_boundary_punctuation(&segment.live_text));
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
    text_config: &TextProcessingConfig,
    segments: &[TranscriptSegment],
    source_id: u64,
    current_partial: &str,
    is_final: bool,
) -> String {
    let source_clauses = source_clause_contexts(segments, source_id);
    let source_match_clauses = source_live_clause_contexts(segments, source_id);
    let anchor_clause_count = source_clauses.len().min(source_match_clauses.len());

    build_refine_display_with_source_context(
        text_config,
        &source_clauses,
        &source_match_clauses,
        anchor_clause_count,
        current_partial,
        is_final,
    )
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

fn next_source_clause_leading_pause_ms(active_refine: &mut ActiveRefineContext) -> u32 {
    if active_refine.committed_clause_count == 0 {
        active_refine.leading_pause_ms
    } else if active_refine.pending_clause_leading_pause_ms != 0 {
        std::mem::take(&mut active_refine.pending_clause_leading_pause_ms)
    } else {
        0
    }
}

fn pause_boundary_punctuation(
    pause_ms: u32,
    text_config: &TextProcessingConfig,
) -> Option<&'static str> {
    if pause_ms < text_config.comma_pause_ms {
        None
    } else if pause_ms < text_config.sentence_pause_ms {
        Some("，")
    } else {
        Some("。")
    }
}

fn arm_pending_vad_pause_boundary(
    text_config: &TextProcessingConfig,
    current_partial: &str,
    pending_pause_boundary: &mut Option<PendingPauseBoundary>,
    pause_ms: u32,
) {
    if pause_boundary_punctuation(pause_ms, text_config).is_none()
        || current_partial.trim().is_empty()
    {
        return;
    }

    *pending_pause_boundary = Some(PendingPauseBoundary {
        pause_ms,
        rough_split_chars: current_partial.chars().count(),
    });
}

fn trim_boundary_prefix(text: &str) -> &str {
    text.trim_start_matches(|ch: char| ch.is_whitespace() || is_boundary_punctuation(ch))
}

fn starts_likely_pause_clause_start(text: &str) -> bool {
    let text = trim_boundary_prefix(text);
    if text.is_empty() {
        return false;
    }

    PAUSE_BOUNDARY_RIGHT_MARKERS
        .iter()
        .any(|marker| text.starts_with(marker))
        || STRONG_CONTINUATION_MARKERS
            .iter()
            .any(|marker| text.starts_with(marker))
}

fn split_text_at_char(text: &str, split_chars: usize) -> (String, String) {
    let split_byte = text
        .char_indices()
        .nth(split_chars)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len());
    (
        text[..split_byte].to_string(),
        text[split_byte..].to_string(),
    )
}

fn resolve_pending_pause_split_chars(text: &str, rough_split_chars: usize) -> Option<usize> {
    let total_chars = text.chars().count();
    if rough_split_chars >= total_chars {
        return None;
    }

    let max_split_chars = rough_split_chars
        .saturating_add(PAUSE_BOUNDARY_MAX_SHIFT_CHARS)
        .min(total_chars.saturating_sub(1));
    let mut fallback_split = None;

    for split_chars in rough_split_chars..=max_split_chars {
        let (_, right) = split_text_at_char(text, split_chars);
        let right = trim_boundary_prefix(&right);
        if right.is_empty() {
            continue;
        }
        if starts_likely_pause_clause_start(right) {
            return Some(split_chars);
        }
        if fallback_split.is_none() && split_chars > rough_split_chars && right.chars().count() >= 2
        {
            fallback_split = Some(split_chars);
        }
    }

    fallback_split
}

fn commit_pending_vad_pause_with_incremental_refine(
    text_config: &TextProcessingConfig,
    current_segment_audio: &[f32],
    current_partial: &mut String,
    current_partial_leading_pause_ms: u32,
    active_refine: &mut Option<ActiveRefineContext>,
    pending_pause_boundary: &mut Option<PendingPauseBoundary>,
    next_refine_source_id: &mut u64,
    segments: &mut Vec<TranscriptSegment>,
    next_segment_id: &mut u64,
) -> bool {
    let Some(pending_boundary) = *pending_pause_boundary else {
        return false;
    };
    let Some(boundary) = pause_boundary_punctuation(pending_boundary.pause_ms, text_config) else {
        *pending_pause_boundary = None;
        return false;
    };
    if current_partial.trim().is_empty() {
        return false;
    }

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
        .expect("active refine context must exist after pause activation");
    if activated_now {
        active.source_audio.extend_from_slice(current_segment_audio);
    }

    let source_match_clauses = source_live_clause_contexts(segments, active.source_id);
    let anchor_clause_count = active
        .committed_clause_count
        .min(source_match_clauses.len());
    let committed_prefix = boundaryless_clause_text(&source_match_clauses[..anchor_clause_count]);
    let remaining_text = remaining_text_after_prefix(&committed_prefix, current_partial);
    let rough_split_chars = pending_boundary
        .rough_split_chars
        .saturating_sub(committed_prefix.chars().count());
    let Some(split_chars) = resolve_pending_pause_split_chars(&remaining_text, rough_split_chars)
    else {
        return false;
    };
    let (commit_text, tail_text) = split_text_at_char(&remaining_text, split_chars);
    if commit_text.trim().is_empty() || tail_text.trim().is_empty() {
        return false;
    }

    let mut tail = commit_text.trim().to_string();
    if !tail.chars().last().is_some_and(is_boundary_punctuation) {
        tail.push_str(boundary);
    }
    push_transcript_segment(
        segments,
        next_segment_id,
        Some(active.source_id),
        Some(active.committed_clause_count),
        next_source_clause_leading_pause_ms(active),
        tail.clone(),
        None,
    );
    active.committed_clause_count += 1;
    active.queued_clause_count = active
        .queued_clause_count
        .max(active.committed_clause_count);
    *pending_pause_boundary = None;
    log::info!(
        "Committed live clause on VAD pause with lookahead: source_id={}, pause_ms={}, rough_split_chars={}, split_chars={}, text='{}'",
        active.source_id,
        pending_boundary.pause_ms,
        pending_boundary.rough_split_chars,
        split_chars,
        tail
    );
    true
}

fn clauses_len(display: &str) -> usize {
    split_stable_clauses(display).0.len()
}

fn rendered_refine_clauses(
    text_config: &TextProcessingConfig,
    text: &str,
    is_final: bool,
    expected_clause_count: Option<usize>,
) -> Vec<String> {
    let rendered = render_refine_result_text(text_config, text, is_final);
    split_rendered_refine_clauses(rendered, is_final, expected_clause_count)
}

fn split_rendered_refine_clauses(
    rendered: String,
    is_final: bool,
    expected_clause_count: Option<usize>,
) -> Vec<String> {
    let (mut clauses, tail) = split_stable_clauses(&rendered);
    if is_final && !tail.is_empty() {
        clauses.push(tail);
    }
    if clauses.is_empty() && !rendered.is_empty() {
        clauses.push(rendered);
    }
    if let Some(expected_clause_count) = expected_clause_count {
        clauses = merge_overflow_refine_clauses(clauses, expected_clause_count);
    }
    clauses
}

fn preferred_pause_preserving_anchor_clause_count(
    source_match_clauses: &[SourceClauseContext],
    text: &str,
) -> usize {
    for anchor_clause_count in (1..=source_match_clauses.len()).rev() {
        let committed_prefix =
            boundaryless_clause_text(&source_match_clauses[..anchor_clause_count]);
        let remaining_text = remaining_text_after_prefix(&committed_prefix, text);
        if remaining_text.is_empty() || starts_likely_pause_clause_start(&remaining_text) {
            return anchor_clause_count;
        }
    }

    0
}

fn merge_overflow_refine_clauses(
    mut clauses: Vec<String>,
    expected_clause_count: usize,
) -> Vec<String> {
    if expected_clause_count == 0 || clauses.len() <= expected_clause_count {
        return clauses;
    }

    if expected_clause_count == 1 {
        return vec![clauses.concat()];
    }

    let overflow = clauses.split_off(expected_clause_count - 1);
    clauses.push(overflow.concat());
    clauses
}

fn upsert_source_clauses_from_text(
    text_config: &TextProcessingConfig,
    segments: &mut Vec<TranscriptSegment>,
    next_segment_id: &mut u64,
    source_id: u64,
    leading_pause_ms: u32,
    text: &str,
    is_final: bool,
) -> usize {
    let clauses = rendered_refine_clauses(text_config, text, is_final, None);
    if clauses.is_empty() {
        return 0;
    }
    let clause_count = clauses.len();

    let has_source_segments = segments.iter().any(|segment| {
        segment.source_id == Some(source_id) && segment.source_clause_index.is_some()
    });

    if has_source_segments {
        let _ = apply_refine_result(
            text_config,
            segments,
            next_segment_id,
            RefineResult {
                source_id,
                text: text.to_string(),
                clause_count,
                is_final,
            },
        );
    } else {
        for (index, clause) in clauses.into_iter().enumerate() {
            push_transcript_segment(
                segments,
                next_segment_id,
                Some(source_id),
                Some(index),
                if index == 0 { leading_pause_ms } else { 0 },
                clause,
                None,
            );
        }
    }

    clause_count
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
    pending_pause_boundary: &mut Option<PendingPauseBoundary>,
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
            let _ = commit_pending_vad_pause_with_incremental_refine(
                text_config,
                current_segment_audio,
                current_partial,
                current_partial_leading_pause_ms,
                active_refine,
                pending_pause_boundary,
                next_refine_source_id,
                segments,
                next_segment_id,
            );
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
                let source_match_clauses = source_live_clause_contexts(segments, active.source_id);
                let display = build_refine_display_with_source_context(
                    text_config,
                    &source_clauses,
                    &source_match_clauses,
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
                        compose_source_fallback_text(
                            text_config,
                            segments,
                            active.source_id,
                            current_partial,
                            false,
                        ),
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
    pending_pause_boundary: &mut Option<PendingPauseBoundary>,
    next_refine_source_id: &mut u64,
    current_partial: &mut String,
    segments: &mut Vec<TranscriptSegment>,
    next_segment_id: &mut u64,
    refine_worker: &mut RefineWorker,
    live_segment_transcriber: Option<&mut OfflineTranscriber>,
) -> Result<(), String> {
    if current_segment_audio.is_empty() && current_partial.is_empty() {
        return Ok(());
    }

    let live_text = live_engine.finalize().map_err(|e| e.to_string())?;
    let mut live_text = if live_text.is_empty() {
        current_partial.clone()
    } else {
        live_text
    };
    if !live_text.is_empty() {
        let mut finalize_partial = live_text.clone();
        let _ = commit_pending_vad_pause_with_incremental_refine(
            text_config,
            current_segment_audio,
            &mut finalize_partial,
            *current_segment_leading_pause_ms,
            active_refine,
            pending_pause_boundary,
            next_refine_source_id,
            segments,
            next_segment_id,
        );
        live_text = finalize_partial;
    }
    current_partial.clear();

    if live_text.is_empty() {
        current_segment_audio.clear();
        *current_segment_leading_pause_ms = 0;
        *pending_pause_boundary = None;
        *active_refine = None;
        sync_runtime_transcript(text_config, segments, None, "", 0);
        return Ok(());
    }

    let overlap_len = current_segment_audio.len().min(SEGMENT_OVERLAP_SAMPLES);
    *segment_overlap_audio =
        current_segment_audio[current_segment_audio.len() - overlap_len..].to_vec();

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
        let final_text = transcribe_live_final_segment(
            live_segment_transcriber,
            &active.source_audio,
            &live_text,
        );
        clause_count = upsert_source_clauses_from_text(
            text_config,
            segments,
            next_segment_id,
            active.source_id,
            active.leading_pause_ms,
            &final_text,
            true,
        );
        if active.source_audio.len() >= MIN_REFINE_SEGMENT_SAMPLES {
            queue_refine_task(
                refine_worker,
                segments,
                active.source_id,
                active.source_audio.clone(),
                final_text,
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
    *pending_pause_boundary = None;
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
    pending_pause_boundary: &mut Option<PendingPauseBoundary>,
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
    *pending_pause_boundary = None;

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
        let source_match_clauses = source_live_clause_contexts(segments, active.source_id);
        let final_display = build_refine_display_with_source_context(
            text_config,
            &source_clauses,
            &source_match_clauses,
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
    live_segment_transcriber: Option<&mut OfflineTranscriber>,
    flush_short_segments: bool,
) -> Result<(), String> {
    if current_segment_audio.is_empty() && current_partial.is_empty() {
        if flush_short_segments {
            commit_pending_segment(
                pending_segment,
                segments,
                next_segment_id,
                refine_worker,
                live_segment_transcriber,
            )?;
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

    live_text = transcribe_live_final_segment(live_segment_transcriber, &segment_audio, &live_text);

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
    live_segment_transcriber: Option<&mut OfflineTranscriber>,
) -> Result<(), String> {
    let Some(pending) = pending_segment.take() else {
        return Ok(());
    };

    let live_text =
        transcribe_live_final_segment(live_segment_transcriber, &pending.audio, &pending.live_text);

    push_segment(
        segments,
        next_segment_id,
        refine_worker,
        pending.leading_pause_ms,
        pending.audio,
        live_text,
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
    segments: &mut Vec<TranscriptSegment>,
    next_segment_id: &mut u64,
) -> bool {
    let mut changed = false;
    for result in drain_latest_refine_results(result_rx, false) {
        changed |= apply_refine_result(text_config, segments, next_segment_id, result);
    }
    changed
}

fn collect_refine_results_blocking(
    text_config: &TextProcessingConfig,
    result_rx: &mpsc::Receiver<RefineResult>,
    segments: &mut Vec<TranscriptSegment>,
    next_segment_id: &mut u64,
) {
    for result in drain_latest_refine_results(result_rx, true) {
        let _ = apply_refine_result(text_config, segments, next_segment_id, result);
    }
}

fn drain_latest_refine_results(
    result_rx: &mpsc::Receiver<RefineResult>,
    wait_for_first: bool,
) -> Vec<RefineResult> {
    let mut latest_by_source = BTreeMap::new();

    if wait_for_first {
        match result_rx.recv_timeout(Duration::from_millis(20)) {
            Ok(result) => absorb_refine_result(&mut latest_by_source, result),
            Err(_) => return Vec::new(),
        }
    }

    while let Ok(result) = result_rx.try_recv() {
        absorb_refine_result(&mut latest_by_source, result);
    }

    latest_by_source.into_values().collect()
}

fn absorb_refine_result(latest_by_source: &mut BTreeMap<u64, RefineResult>, result: RefineResult) {
    let should_replace = latest_by_source
        .get(&result.source_id)
        .map(|existing| {
            result.clause_count > existing.clause_count
                || (result.clause_count == existing.clause_count
                    && result.is_final
                    && !existing.is_final)
        })
        .unwrap_or(true);

    if should_replace {
        latest_by_source.insert(result.source_id, result);
    }
}

fn apply_refine_result(
    text_config: &TextProcessingConfig,
    segments: &mut Vec<TranscriptSegment>,
    next_segment_id: &mut u64,
    result: RefineResult,
) -> bool {
    let source_clauses = source_clause_contexts(segments, result.source_id);
    let source_match_clauses = source_live_clause_contexts(segments, result.source_id);
    let clauses = if result.clause_count == 1
        && !source_clauses.is_empty()
        && !source_match_clauses.is_empty()
    {
        let anchor_clause_count =
            preferred_pause_preserving_anchor_clause_count(&source_match_clauses, &result.text);
        if anchor_clause_count == 0 {
            rendered_refine_clauses(
                text_config,
                &result.text,
                result.is_final,
                Some(result.clause_count),
            )
        } else {
            let rendered = build_refine_display_with_source_context(
                text_config,
                &source_clauses,
                &source_match_clauses,
                anchor_clause_count,
                &result.text,
                result.is_final,
            );
            log::info!(
                "Refine result preserved committed source prefix: source_id={}, final={}, anchor_clause_count={}, rendered='{}'",
                result.source_id,
                result.is_final,
                anchor_clause_count,
                rendered
            );
            split_rendered_refine_clauses(rendered, result.is_final, None)
        }
    } else {
        rendered_refine_clauses(
            text_config,
            &result.text,
            result.is_final,
            Some(result.clause_count),
        )
    };
    let apply_count = clauses.len();
    log::info!(
        "Refine result received: source_id={}, final={}, requested_clause_count={}, parsed_clause_count={}, rendered='{}'",
        result.source_id,
        result.is_final,
        result.clause_count,
        clauses.len(),
        clauses.join("")
    );
    let mut applied = 0usize;
    let mut source_segments = segments
        .iter()
        .enumerate()
        .filter_map(|(segment_idx, segment)| {
            (segment.source_id == Some(result.source_id))
                .then_some(segment.source_clause_index)
                .flatten()
                .map(|clause_index| (segment_idx, clause_index))
        })
        .collect::<Vec<_>>();
    source_segments.sort_by_key(|(_, clause_index)| *clause_index);

    let existing_clause_count = source_segments.len();
    for (segment_idx, clause_index) in source_segments {
        let segment = &mut segments[segment_idx];
        let before = segment
            .corrected_text
            .as_deref()
            .unwrap_or(segment.live_text.as_str())
            .to_string();

        if clause_index < apply_count {
            let corrected = clauses[clause_index].clone();
            if segment.corrected_text.as_deref() != Some(corrected.as_str()) {
                segment.corrected_text = Some(corrected);
                applied += 1;
                log::info!(
                    "Refine clause updated: source_id={}, clause_index={}, segment_id={}, before='{}', after='{}'",
                    result.source_id,
                    clause_index,
                    segment.id,
                    before,
                    segment.corrected_text.as_deref().unwrap_or("")
                );
            } else {
                log::info!(
                    "Refine clause unchanged: source_id={}, clause_index={}, segment_id={}, text='{}'",
                    result.source_id,
                    clause_index,
                    segment.id,
                    before
                );
            }
            continue;
        }

        if result.is_final && segment.corrected_text.as_deref() != Some("") {
            segment.corrected_text = Some(String::new());
            applied += 1;
            log::info!(
                "Refine clause cleared: source_id={}, clause_index={}, segment_id={}, before='{}'",
                result.source_id,
                clause_index,
                segment.id,
                before
            );
        }
    }

    for clause_index in existing_clause_count..apply_count {
        let clause = clauses[clause_index].clone();
        push_transcript_segment(
            segments,
            next_segment_id,
            Some(result.source_id),
            Some(clause_index),
            0,
            clause.clone(),
            Some(clause.clone()),
        );
        applied += 1;
        log::info!(
            "Refine clause appended: source_id={}, clause_index={}, text='{}'",
            result.source_id,
            clause_index,
            clause
        );
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

fn render_refine_result_text(
    text_config: &TextProcessingConfig,
    text: &str,
    is_final: bool,
) -> String {
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }

    if contains_refine_boundary_punctuation(text) {
        return if is_final {
            finalize_existing_punctuation_text(text, text_config)
        } else {
            normalize_existing_punctuation_text(text, text_config)
        };
    }

    render_segmented_transcript(
        &[],
        None,
        Some(TranscriptChunk {
            text,
            leading_pause_ms: 0,
        }),
        text_config,
        is_final,
    )
}

fn contains_refine_boundary_punctuation(text: &str) -> bool {
    text.chars().any(|ch| {
        matches!(
            ch,
            '，' | '。' | '？' | '、' | ',' | '.' | '?' | '!' | '！' | ';' | '；' | ':'
        )
    })
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
    if !config.rewrite.enabled {
        return None;
    }

    if config.rewrite.providers.is_empty() && config.rewrite.active_provider_id.is_empty() {
        return None;
    }

    match llm::create_client_from_app_config(config) {
        Ok(client) => {
            state::set_state(AppState::Rewriting);
            state::set_status_message("LLM润色中...");
            match client.rewrite(text) {
                Ok(rewritten) if !rewritten.is_empty() => Some(rewritten),
                Ok(_) => None,
                Err(err) => {
                    state::set_status_message(format!("LLM 润色失败，已保留原文: {}", err));
                    None
                }
            }
        }
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
        arm_pending_vad_pause_boundary, build_refine_display_with_source_context,
        commit_pending_vad_pause_with_incremental_refine, compose_source_fallback_text,
        effective_segment_leading_pause_ms, ensure_active_refine_context_for_partial,
        pause_boundary_punctuation, preferred_pause_preserving_anchor_clause_count,
        remaining_display_after_clause_count, remaining_text_after_prefix,
        resolve_pending_pause_split_chars, source_clause_contexts, source_live_clause_contexts,
        split_stable_clauses, ActiveRefineContext, PendingPauseBoundary, RefineAudioStats,
        RefineResult, TextProcessingConfig, TranscriptSegment,
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
    fn split_stable_clauses_skips_punctuation_only_fragments() {
        let (clauses, tail) = split_stable_clauses("第一句，，第二句");
        assert_eq!(clauses, vec!["第一句，"]);
        assert_eq!(tail, "第二句");
    }

    #[test]
    fn remaining_display_skips_committed_clauses() {
        let tail =
            remaining_display_after_clause_count("中文流式语音识别模型，适合低延迟实时转", 1);
        assert_eq!(tail, "适合低延迟实时转");
    }

    #[test]
    fn pause_boundary_uses_comma_for_medium_pause() {
        let text_config = TextProcessingConfig::default();

        assert_eq!(pause_boundary_punctuation(640, &text_config), None);
        assert_eq!(pause_boundary_punctuation(800, &text_config), Some("，"));
    }

    #[test]
    fn pending_pause_split_extends_to_next_clause_start_marker() {
        assert_eq!(
            resolve_pending_pause_split_chars("音频在本地处理不上", "音频在本地处".chars().count()),
            Some("音频在本地处理".chars().count())
        );
        assert_eq!(
            resolve_pending_pause_split_chars(
                "不上传任何语音数据隐",
                "不上传任何语音数".chars().count()
            ),
            Some("不上传任何语音数据".chars().count())
        );
    }

    #[test]
    fn pending_pause_arms_from_current_partial_length() {
        let text_config = TextProcessingConfig::default();
        let mut pending_pause_boundary = None;

        arm_pending_vad_pause_boundary(
            &text_config,
            "音频在本地处",
            &mut pending_pause_boundary,
            832,
        );

        assert_eq!(
            pending_pause_boundary,
            Some(PendingPauseBoundary {
                pause_ms: 832,
                rough_split_chars: "音频在本地处".chars().count(),
            })
        );
    }

    #[test]
    fn pending_pause_commits_clause_after_lookahead() {
        let text_config = TextProcessingConfig::default();
        let mut segments = Vec::new();
        let mut active_refine = None;
        let mut pending_pause_boundary = Some(PendingPauseBoundary {
            pause_ms: 832,
            rough_split_chars: "音频在本地处".chars().count(),
        });
        let mut next_refine_source_id = 8;
        let mut next_segment_id = 1;
        let mut current_partial = "音频在本地处理不上".to_string();

        let committed = commit_pending_vad_pause_with_incremental_refine(
            &text_config,
            &[],
            &mut current_partial,
            1472,
            &mut active_refine,
            &mut pending_pause_boundary,
            &mut next_refine_source_id,
            &mut segments,
            &mut next_segment_id,
        );

        assert!(committed);
        assert_eq!(pending_pause_boundary, None);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].source_id, Some(8));
        assert_eq!(segments[0].source_clause_index, Some(0));
        assert_eq!(segments[0].live_text, "音频在本地处理，");
        assert_eq!(
            active_refine
                .as_ref()
                .expect("active refine should remain open")
                .committed_clause_count,
            1
        );
        assert_eq!(
            active_refine
                .as_ref()
                .expect("active refine should remain open")
                .queued_clause_count,
            1
        );
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
        let mut next_segment_id = 4;

        apply_refine_result(
            &text_config,
            &mut segments,
            &mut next_segment_id,
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
    fn single_clause_refine_result_keeps_first_committed_pause_boundary() {
        let text_config = TextProcessingConfig::default();
        let mut segments = vec![
            TranscriptSegment {
                id: 1,
                source_id: Some(16),
                source_clause_index: Some(0),
                leading_pause_ms: 1472,
                live_text: "音频在本地处理，".to_string(),
                corrected_text: None,
            },
            TranscriptSegment {
                id: 2,
                source_id: Some(16),
                source_clause_index: Some(1),
                leading_pause_ms: 0,
                live_text: "不上传任何语音数，".to_string(),
                corrected_text: None,
            },
        ];
        let mut next_segment_id = 3;

        apply_refine_result(
            &text_config,
            &mut segments,
            &mut next_segment_id,
            RefineResult {
                source_id: 16,
                text: "音频在本地处理不上传任何语音数据隐私完全可控。".to_string(),
                clause_count: 1,
                is_final: true,
            },
        );

        assert_eq!(segments.len(), 2);
        assert_eq!(
            segments[0].corrected_text.as_deref(),
            Some("音频在本地处理，")
        );
        assert_eq!(
            segments[1].corrected_text.as_deref(),
            Some("不上传任何语音数据隐私完全可控。")
        );
    }

    #[test]
    fn final_single_clause_refine_can_preserve_multiple_pause_boundaries() {
        let source_match_clauses = vec![
            super::SourceClauseContext {
                text: "音频在本地处理，".to_string(),
                leading_pause_ms: 1472,
            },
            super::SourceClauseContext {
                text: "不上传任何语音数据，".to_string(),
                leading_pause_ms: 0,
            },
        ];

        let anchor_clause_count = preferred_pause_preserving_anchor_clause_count(
            &source_match_clauses,
            "音频在本地处理不上传任何语音数据隐私完全可控。",
        );

        assert_eq!(anchor_clause_count, 2);
    }

    #[test]
    fn refine_result_does_not_duplicate_when_prefix_changes() {
        let text_config = TextProcessingConfig::default();
        let mut segments = vec![
            TranscriptSegment {
                id: 1,
                source_id: Some(9),
                source_clause_index: Some(0),
                leading_pause_ms: 0,
                live_text: "就是柯尼曼写的那一本人的两种思维模式里面有个概念叫system一和system二system一是，"
                    .to_string(),
                corrected_text: None,
            },
            TranscriptSegment {
                id: 2,
                source_id: Some(9),
                source_clause_index: Some(1),
                leading_pause_ms: 0,
                live_text: "快速直觉反".to_string(),
                corrected_text: None,
            },
        ];
        let mut next_segment_id = 3;

        apply_refine_result(
            &text_config,
            &mut segments,
            &mut next_segment_id,
            RefineResult {
                source_id: 9,
                text: "就是科尼曼写的那本讲人的两种思维模式里面有个概念叫system一和system二system一是快速直觉反应"
                    .to_string(),
                clause_count: 2,
                is_final: false,
            },
        );

        let combined = segments
            .iter()
            .map(|segment| {
                segment
                    .corrected_text
                    .as_deref()
                    .unwrap_or(segment.live_text.as_str())
            })
            .collect::<String>();
        assert_eq!(combined.matches("科尼曼写的那本讲人的").count(), 1);
        assert!(combined.contains("快速直觉反"));
    }

    #[test]
    fn refine_result_final_clears_extra_source_clauses() {
        let text_config = TextProcessingConfig::default();
        let mut segments = vec![
            TranscriptSegment {
                id: 1,
                source_id: Some(10),
                source_clause_index: Some(0),
                leading_pause_ms: 0,
                live_text: "第一句，".to_string(),
                corrected_text: None,
            },
            TranscriptSegment {
                id: 2,
                source_id: Some(10),
                source_clause_index: Some(1),
                leading_pause_ms: 0,
                live_text: "，".to_string(),
                corrected_text: None,
            },
            TranscriptSegment {
                id: 3,
                source_id: Some(10),
                source_clause_index: Some(2),
                leading_pause_ms: 0,
                live_text: "第二句。".to_string(),
                corrected_text: None,
            },
        ];
        let mut next_segment_id = 4;

        apply_refine_result(
            &text_config,
            &mut segments,
            &mut next_segment_id,
            RefineResult {
                source_id: 10,
                text: "第一句第二句".to_string(),
                clause_count: 3,
                is_final: true,
            },
        );

        assert_eq!(
            segments[0].corrected_text.as_deref(),
            Some("第一句第二句。")
        );
        assert_eq!(segments[1].corrected_text.as_deref(), Some(""));
        assert_eq!(segments[2].corrected_text.as_deref(), Some(""));
    }

    #[test]
    fn refine_result_merges_overflow_clauses_into_last_requested_clause() {
        let text_config = TextProcessingConfig::default();
        let mut segments = vec![
            TranscriptSegment {
                id: 1,
                source_id: Some(13),
                source_clause_index: Some(0),
                leading_pause_ms: 0,
                live_text: "最近在看一本书叫思考快与慢，".to_string(),
                corrected_text: None,
            },
            TranscriptSegment {
                id: 2,
                source_id: Some(13),
                source_clause_index: Some(1),
                leading_pause_ms: 0,
                live_text: "就是卡尼曼。".to_string(),
                corrected_text: None,
            },
        ];
        let mut next_segment_id = 3;

        apply_refine_result(
            &text_config,
            &mut segments,
            &mut next_segment_id,
            RefineResult {
                source_id: 13,
                text: "最近在看一本书叫思考，快与慢，就是卡尼曼。".to_string(),
                clause_count: 2,
                is_final: false,
            },
        );

        assert_eq!(
            segments[0].corrected_text.as_deref(),
            Some("最近在看一本书叫思考，")
        );
        assert_eq!(
            segments[1].corrected_text.as_deref(),
            Some("快与慢，就是卡尼曼。")
        );
    }

    #[test]
    fn refine_result_repairs_missing_internal_boundary_from_punct_model_output() {
        let text_config = TextProcessingConfig::default();
        let mut segments = vec![
            TranscriptSegment {
                id: 1,
                source_id: Some(15),
                source_clause_index: Some(0),
                leading_pause_ms: 0,
                live_text: "中文流式语音识别模型，".to_string(),
                corrected_text: None,
            },
            TranscriptSegment {
                id: 2,
                source_id: Some(15),
                source_clause_index: Some(1),
                leading_pause_ms: 0,
                live_text: "适合低延迟实时转写。".to_string(),
                corrected_text: None,
            },
        ];
        let mut next_segment_id = 3;

        apply_refine_result(
            &text_config,
            &mut segments,
            &mut next_segment_id,
            RefineResult {
                source_id: 15,
                text: "中文流式语音识别模型适合低延迟实时转写。".to_string(),
                clause_count: 2,
                is_final: false,
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
    }

    #[test]
    fn refine_result_appends_missing_source_clauses() {
        let text_config = TextProcessingConfig::default();
        let mut segments = vec![
            TranscriptSegment {
                id: 1,
                source_id: Some(14),
                source_clause_index: Some(0),
                leading_pause_ms: 0,
                live_text: "最近在看一本书叫思考，".to_string(),
                corrected_text: None,
            },
            TranscriptSegment {
                id: 2,
                source_id: Some(14),
                source_clause_index: Some(1),
                leading_pause_ms: 0,
                live_text: "快与慢，".to_string(),
                corrected_text: None,
            },
        ];
        let mut next_segment_id = 3;

        apply_refine_result(
            &text_config,
            &mut segments,
            &mut next_segment_id,
            RefineResult {
                source_id: 14,
                text: "最近在看一本书叫思考，快与慢，就是卡尼曼写的那本书。".to_string(),
                clause_count: 3,
                is_final: true,
            },
        );

        assert_eq!(segments.len(), 3);
        assert_eq!(segments[2].source_id, Some(14));
        assert_eq!(segments[2].source_clause_index, Some(2));
        assert_eq!(
            segments[2].corrected_text.as_deref(),
            Some("就是卡尼曼写的那本书。")
        );
    }

    #[test]
    fn compose_source_fallback_text_does_not_duplicate_committed_prefix() {
        let text_config = TextProcessingConfig::default();
        let segments = vec![TranscriptSegment {
            id: 1,
            source_id: Some(7),
            source_clause_index: Some(0),
            leading_pause_ms: 0,
            live_text: "中文流式语音识别模型，".to_string(),
            corrected_text: None,
        }];

        let result = compose_source_fallback_text(
            &text_config,
            &segments,
            7,
            "中文流式语音识别模型适合低延迟实时转写",
            false,
        );

        assert_eq!(result, "中文流式语音识别模型，适合低延迟实时转写");
    }

    #[test]
    fn compose_source_fallback_text_uses_live_prefix_when_corrected_prefix_changes() {
        let text_config = TextProcessingConfig::default();
        let segments = vec![TranscriptSegment {
            id: 1,
            source_id: Some(12),
            source_clause_index: Some(0),
            leading_pause_ms: 0,
            live_text: "有个概念sysystem一和system二system一是，".to_string(),
            corrected_text: Some("面有个概念叫system一和system二system一是，".to_string()),
        }];

        let result = compose_source_fallback_text(
            &text_config,
            &segments,
            12,
            "有个概念sysystem一和system二system一是快速直觉反应",
            false,
        );

        assert_eq!(
            result,
            "面有个概念叫system一和system二system一是，快速直觉反应"
        );
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
        let source_match_clauses = source_live_clause_contexts(&segments, 7);
        let rendered = build_refine_display_with_source_context(
            &text_config,
            &source_clauses,
            &source_match_clauses,
            1,
            "中文流式语音识别模型适合低延迟实时转写",
            false,
        );

        assert_eq!(rendered, "中文流式语音识别模型，适合低延迟实时转写");
    }

    #[test]
    fn refine_display_with_corrected_prefix_uses_live_prefix_for_tail_matching() {
        let text_config = TextProcessingConfig::default();
        let segments = vec![TranscriptSegment {
            id: 1,
            source_id: Some(11),
            source_clause_index: Some(0),
            leading_pause_ms: 0,
            live_text: "有个概念sysystem一和system二system一是，".to_string(),
            corrected_text: Some("面有个概念叫system一和system二system一是，".to_string()),
        }];

        let source_clauses = source_clause_contexts(&segments, 11);
        let source_match_clauses = source_live_clause_contexts(&segments, 11);
        let rendered = build_refine_display_with_source_context(
            &text_config,
            &source_clauses,
            &source_match_clauses,
            1,
            "有个概念sysystem一和system二system一是快速直觉反",
            false,
        );

        assert_eq!(
            rendered,
            "面有个概念叫system一和system二system一是，快速直觉反"
        );
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

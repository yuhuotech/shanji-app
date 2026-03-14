use crate::denoiser::Denoiser;
use crate::error::{AppError, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

const TARGET_SAMPLE_RATE: u32 = 16_000;
const TARGET_48K: u32 = 48_000;
const RESAMPLE_BUFFER_SIZE: usize = 1024;

// ── macOS microphone permission ───────────────────────────────────────────────

#[cfg(target_os = "macos")]
#[link(name = "AVFoundation", kind = "framework")]
extern "C" {}

#[derive(Debug, Clone, PartialEq)]
pub enum MicPermissionStatus {
    /// Permission granted
    Authorized,
    /// User explicitly denied
    Denied,
    /// Not yet asked
    NotDetermined,
    /// Restricted by MDM/parental controls
    Restricted,
}

/// Returns the current macOS microphone authorization status.
#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)] // objc 0.2 macros use cfg(cargo-clippy) internally
pub fn get_mic_permission_status() -> MicPermissionStatus {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    unsafe {
        // AVMediaTypeAudio constant = @"soun"
        let ns_string_cls = class!(NSString);
        let media_type: *const Object =
            msg_send![ns_string_cls, stringWithUTF8String: b"soun\0".as_ptr()];

        let av_cls = class!(AVCaptureDevice);
        let status: i64 = msg_send![av_cls, authorizationStatusForMediaType: media_type];

        match status {
            3 => MicPermissionStatus::Authorized,
            2 => MicPermissionStatus::Denied,
            1 => MicPermissionStatus::Restricted,
            _ => MicPermissionStatus::NotDetermined,
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn get_mic_permission_status() -> MicPermissionStatus {
    // On non-macOS platforms we assume permission is granted if devices exist.
    MicPermissionStatus::Authorized
}

/// Opens System Settings → Privacy → Microphone so the user can grant access.
pub fn open_mic_permission_settings() {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
            .spawn();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioInputDevice {
    pub name: String,
    pub is_default: bool,
}

pub fn list_input_devices() -> Result<Vec<AudioInputDevice>> {
    let host = cpal::default_host();
    let default_device = host.default_input_device();
    let default_name = default_device
        .as_ref()
        .and_then(|device| device.name().ok());

    let mut devices = Vec::new();

    for device in host
        .input_devices()
        .map_err(|e| AppError::Audio(e.to_string()))?
    {
        if let Ok(name) = device.name() {
            devices.push(AudioInputDevice {
                is_default: Some(name.as_str()) == default_name.as_deref(),
                name,
            });
        }
    }

    Ok(devices)
}

pub fn calculate_audio_level(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }

    let sum_squares: f32 = samples.iter().map(|sample| sample * sample).sum();
    let rms = (sum_squares / samples.len() as f32).sqrt();
    (rms * 10.0).min(1.0)
}

struct CaptureState {
    sample_tx: Sender<Vec<f32>>,
    input_buffer: Vec<f32>,
    /// noise_reduction=false 时：native→16kHz（原有逻辑）
    resampler_to_16k: Option<SincFixedIn<f32>>,
    /// noise_reduction=true 时：native→48kHz（若 mic 已是 48kHz 则 None）
    resampler_to_48k: Option<SincFixedIn<f32>>,
    /// noise_reduction=true 时：48kHz→16kHz（始终创建）
    resampler_48k_to_16k: Option<SincFixedIn<f32>>,
    /// DNN 降噪器（noise_reduction=true 时 Some）
    denoiser: Option<Denoiser>,
    running: Arc<AtomicBool>,
}

pub struct AudioCapture {
    stream: Option<cpal::Stream>,
    running: Arc<AtomicBool>,
}

fn make_resampler(from: u32, to: u32, buffer_size: usize) -> Result<SincFixedIn<f32>> {
    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 128,
        window: WindowFunction::BlackmanHarris2,
    };
    SincFixedIn::<f32>::new(to as f64 / from as f64, 2.0, params, buffer_size, 1)
        .map_err(|e| AppError::Audio(format!("Failed to create resampler {}→{}: {}", from, to, e)))
}

fn resample_chunk(buffer: &mut Vec<f32>, resampler: &mut SincFixedIn<f32>) -> Vec<f32> {
    let mut out = Vec::new();
    while buffer.len() >= RESAMPLE_BUFFER_SIZE {
        let chunk: Vec<f32> = buffer.drain(..RESAMPLE_BUFFER_SIZE).collect();
        match resampler.process(&[chunk], None) {
            Ok(output) => {
                if let Some(ch) = output.first() {
                    out.extend_from_slice(ch);
                }
            }
            Err(e) => log::error!("Resampling error: {:?}", e),
        }
    }
    out
}

impl AudioCapture {
    pub fn new() -> Self {
        Self {
            stream: None,
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn start(
        &mut self,
        device_name: Option<&str>,
        sample_tx: Sender<Vec<f32>>,
        noise_reduction: bool,
    ) -> Result<()> {
        let host = cpal::default_host();

        let device = if let Some(name) = device_name {
            host.input_devices()
                .map_err(|e| AppError::Audio(format!("Failed to list devices: {}", e)))?
                .find(|device| device.name().map(|value| value == name).unwrap_or(false))
                .ok_or_else(|| AppError::Audio(format!("Device not found: {}", name)))?
        } else {
            host.default_input_device()
                .ok_or_else(|| AppError::Audio("No default input device available".to_string()))?
        };

        let default_config = device
            .default_input_config()
            .map_err(|e| AppError::Audio(format!("Failed to get default config: {}", e)))?;

        let input_sample_rate = default_config.sample_rate().0;
        let channels = default_config.channels();

        let (resampler_to_16k, resampler_to_48k, resampler_48k_to_16k, denoiser) =
            if noise_reduction {
                // 新路径：native → 48kHz → denoise → 16kHz
                let to_48k = if input_sample_rate != TARGET_48K {
                    Some(make_resampler(
                        input_sample_rate,
                        TARGET_48K,
                        RESAMPLE_BUFFER_SIZE,
                    )?)
                } else {
                    None // mic 已是 48kHz，直通
                };
                let to_16k = make_resampler(TARGET_48K, TARGET_SAMPLE_RATE, RESAMPLE_BUFFER_SIZE)?;
                (None, to_48k, Some(to_16k), Some(Denoiser::new()))
            } else {
                // 原有路径：native → 16kHz
                let to_16k = if input_sample_rate != TARGET_SAMPLE_RATE {
                    Some(make_resampler(
                        input_sample_rate,
                        TARGET_SAMPLE_RATE,
                        RESAMPLE_BUFFER_SIZE,
                    )?)
                } else {
                    None
                };
                (to_16k, None, None, None)
            };

        self.running.store(true, Ordering::SeqCst);
        let state = Arc::new(std::sync::Mutex::new(CaptureState {
            sample_tx,
            input_buffer: Vec::with_capacity(RESAMPLE_BUFFER_SIZE * channels as usize),
            resampler_to_16k,
            resampler_to_48k,
            resampler_48k_to_16k,
            denoiser,
            running: self.running.clone(),
        }));

        let stream_config: cpal::StreamConfig = default_config.clone().into();
        let stream = match default_config.sample_format() {
            SampleFormat::F32 => {
                self.build_stream::<f32>(&device, &stream_config, channels, state)?
            }
            SampleFormat::I16 => {
                self.build_stream::<i16>(&device, &stream_config, channels, state)?
            }
            SampleFormat::U16 => {
                self.build_stream::<u16>(&device, &stream_config, channels, state)?
            }
            _ => {
                return Err(AppError::Audio(format!(
                    "Unsupported sample format: {:?}",
                    default_config.sample_format()
                )))
            }
        };

        stream
            .play()
            .map_err(|e| AppError::Audio(format!("Failed to start stream: {}", e)))?;

        self.stream = Some(stream);
        Ok(())
    }

    pub fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        self.stream = None;
    }

    fn build_stream<T>(
        &self,
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        channels: u16,
        state: Arc<std::sync::Mutex<CaptureState>>,
    ) -> Result<cpal::Stream>
    where
        T: cpal::SizedSample + cpal::FromSample<T>,
        f32: cpal::FromSample<T>,
    {
        let sample_rate = config.sample_rate.0;
        let channel_count = channels as usize;

        device
            .build_input_stream(
                config,
                move |data: &[T], _: &cpal::InputCallbackInfo| {
                    if !state.lock().unwrap().running.load(Ordering::SeqCst) {
                        return;
                    }
                    Self::process_input_data(data, channel_count, sample_rate, &state);
                },
                move |err| {
                    log::error!("Audio stream error: {}", err);
                },
                None,
            )
            .map_err(|e| AppError::Audio(format!("Failed to build stream: {}", e)))
    }

    fn process_input_data<T>(
        data: &[T],
        channels: usize,
        _sample_rate: u32,
        state: &Arc<std::sync::Mutex<CaptureState>>,
    ) where
        T: cpal::Sample + cpal::FromSample<T>,
        f32: cpal::FromSample<T>,
    {
        let mut state = state.lock().unwrap();

        // 声道下混为单声道
        if channels == 1 {
            for sample in data {
                state.input_buffer.push(sample.to_sample::<f32>());
            }
        } else {
            for chunk in data.chunks(channels) {
                let sum: f32 = chunk.iter().map(|sample| sample.to_sample::<f32>()).sum();
                state.input_buffer.push(sum / channels as f32);
            }
        }

        if state.denoiser.is_some() {
            // 新路径：native → 48kHz → denoise → 16kHz → send
            // 使用 take/put-back 模式规避借用检查
            let samples_48k = if let Some(mut r) = state.resampler_to_48k.take() {
                let out = resample_chunk(&mut state.input_buffer, &mut r);
                state.resampler_to_48k = Some(r);
                out
            } else {
                // mic 已是 48kHz，直通
                std::mem::take(&mut state.input_buffer)
            };

            if !samples_48k.is_empty() {
                let denoised = {
                    let d = state.denoiser.as_mut().unwrap();
                    d.process(&samples_48k)
                };
                let mut denoised_buf = denoised;

                if let Some(mut r) = state.resampler_48k_to_16k.take() {
                    while denoised_buf.len() >= RESAMPLE_BUFFER_SIZE {
                        let chunk: Vec<f32> =
                            denoised_buf.drain(..RESAMPLE_BUFFER_SIZE).collect();
                        match r.process(&[chunk], None) {
                            Ok(out) => {
                                if let Some(ch) = out.first() {
                                    if !ch.is_empty() {
                                        let _ = state.sample_tx.send(ch.clone());
                                    }
                                }
                            }
                            Err(e) => log::error!("Resampling 48k→16k error: {:?}", e),
                        }
                    }
                    state.resampler_48k_to_16k = Some(r);
                }
            }
        } else {
            // 原有路径：native → 16kHz → send
            if let Some(mut resampler) = state.resampler_to_16k.take() {
                while state.input_buffer.len() >= RESAMPLE_BUFFER_SIZE {
                    let chunk: Vec<f32> =
                        state.input_buffer.drain(..RESAMPLE_BUFFER_SIZE).collect();
                    match resampler.process(&[chunk], None) {
                        Ok(output) => {
                            if let Some(ch) = output.first() {
                                if !ch.is_empty() {
                                    let _ = state.sample_tx.send(ch.clone());
                                }
                            }
                        }
                        Err(err) => {
                            log::error!("Resampling error: {:?}", err);
                        }
                    }
                }
                state.resampler_to_16k = Some(resampler);
            } else {
                const CHUNK_SIZE: usize = 1600;
                while state.input_buffer.len() >= CHUNK_SIZE {
                    let chunk: Vec<f32> = state.input_buffer.drain(..CHUNK_SIZE).collect();
                    let _ = state.sample_tx.send(chunk);
                }
            }
        }
    }
}

impl Default for AudioCapture {
    fn default() -> Self {
        Self::new()
    }
}

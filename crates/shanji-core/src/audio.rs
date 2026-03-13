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
const RESAMPLE_BUFFER_SIZE: usize = 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioInputDevice {
    pub name: String,
    pub is_default: bool,
}

pub fn list_input_devices() -> Result<Vec<AudioInputDevice>> {
    let host = cpal::default_host();
    let default_device = host.default_input_device();
    let default_name = default_device.as_ref().and_then(|device| device.name().ok());

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
    resampler: Option<SincFixedIn<f32>>,
    running: Arc<AtomicBool>,
}

pub struct AudioCapture {
    stream: Option<cpal::Stream>,
    running: Arc<AtomicBool>,
}

impl AudioCapture {
    pub fn new() -> Self {
        Self {
            stream: None,
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn start(&mut self, device_name: Option<&str>, sample_tx: Sender<Vec<f32>>) -> Result<()> {
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
        let resampler = if input_sample_rate != TARGET_SAMPLE_RATE {
            let params = SincInterpolationParameters {
                sinc_len: 256,
                f_cutoff: 0.95,
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: 128,
                window: WindowFunction::BlackmanHarris2,
            };

            Some(
                SincFixedIn::<f32>::new(
                    TARGET_SAMPLE_RATE as f64 / input_sample_rate as f64,
                    2.0,
                    params,
                    RESAMPLE_BUFFER_SIZE,
                    1,
                )
                .map_err(|e| AppError::Audio(format!("Failed to create resampler: {}", e)))?,
            )
        } else {
            None
        };

        self.running.store(true, Ordering::SeqCst);
        let state = Arc::new(std::sync::Mutex::new(CaptureState {
            sample_tx,
            input_buffer: Vec::with_capacity(RESAMPLE_BUFFER_SIZE * channels as usize),
            resampler,
            running: self.running.clone(),
        }));

        let stream_config: cpal::StreamConfig = default_config.clone().into();
        let stream = match default_config.sample_format() {
            SampleFormat::F32 => self.build_stream::<f32>(&device, &stream_config, channels, state)?,
            SampleFormat::I16 => self.build_stream::<i16>(&device, &stream_config, channels, state)?,
            SampleFormat::U16 => self.build_stream::<u16>(&device, &stream_config, channels, state)?,
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
        sample_rate: u32,
        state: &Arc<std::sync::Mutex<CaptureState>>,
    ) where
        T: cpal::Sample + cpal::FromSample<T>,
        f32: cpal::FromSample<T>,
    {
        let mut state = state.lock().unwrap();

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

        if sample_rate != TARGET_SAMPLE_RATE {
            if let Some(mut resampler) = state.resampler.take() {
                while state.input_buffer.len() >= RESAMPLE_BUFFER_SIZE {
                    let input_chunk: Vec<f32> =
                        state.input_buffer.drain(..RESAMPLE_BUFFER_SIZE).collect();

                    match resampler.process(&[input_chunk], None) {
                        Ok(output) => {
                            if let Some(channel) = output.first() {
                                if !channel.is_empty() {
                                    let _ = state.sample_tx.send(channel.clone());
                                }
                            }
                        }
                        Err(err) => {
                            log::error!("Resampling error: {:?}", err);
                        }
                    }
                }
                state.resampler = Some(resampler);
            } else {
                let samples = state.input_buffer.drain(..).collect::<Vec<f32>>();
                let _ = state.sample_tx.send(samples);
            }
        } else {
            const CHUNK_SIZE: usize = 1600;
            while state.input_buffer.len() >= CHUNK_SIZE {
                let chunk: Vec<f32> = state.input_buffer.drain(..CHUNK_SIZE).collect();
                let _ = state.sample_tx.send(chunk);
            }
        }
    }
}

impl Default for AudioCapture {
    fn default() -> Self {
        Self::new()
    }
}

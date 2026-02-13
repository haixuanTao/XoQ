//! Audio device abstraction using cpal.
//!
//! Provides cross-platform audio I/O (ALSA, CoreAudio, WASAPI) for
//! capturing from microphones and playing to speakers.

use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Audio sample format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    /// 16-bit signed integer
    I16,
    /// 32-bit float
    F32,
}

impl SampleFormat {
    /// Bytes per sample for this format.
    pub fn bytes_per_sample(&self) -> usize {
        match self {
            SampleFormat::I16 => 2,
            SampleFormat::F32 => 4,
        }
    }

    /// Wire encoding value.
    pub fn to_wire(&self) -> u16 {
        match self {
            SampleFormat::I16 => 0,
            SampleFormat::F32 => 1,
        }
    }

    /// Decode from wire encoding.
    pub fn from_wire(v: u16) -> Result<Self> {
        match v {
            0 => Ok(SampleFormat::I16),
            1 => Ok(SampleFormat::F32),
            _ => anyhow::bail!("Unknown sample format: {}", v),
        }
    }
}

/// Audio configuration (sample rate, channels, format).
#[derive(Debug, Clone)]
pub struct AudioConfig {
    pub sample_rate: u32,
    pub channels: u16,
    pub sample_format: SampleFormat,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 1,
            sample_format: SampleFormat::I16,
        }
    }
}

/// A single chunk of audio data.
#[derive(Debug, Clone)]
pub struct AudioFrame {
    pub data: Vec<u8>,
    pub frame_count: u32,
    pub timestamp_us: u64,
    pub config: AudioConfig,
}

/// Wire header size for iroh transport (24 bytes).
pub const WIRE_HEADER_SIZE: usize = 24;

/// Wire header size for MoQ transport (20 bytes, truncated timestamp).
pub const MOQ_HEADER_SIZE: usize = 20;

impl AudioFrame {
    /// Encode the frame header for iroh wire protocol (24 bytes).
    pub fn encode_header(&self) -> [u8; WIRE_HEADER_SIZE] {
        let mut header = [0u8; WIRE_HEADER_SIZE];
        header[0..4].copy_from_slice(&self.config.sample_rate.to_le_bytes());
        header[4..6].copy_from_slice(&self.config.channels.to_le_bytes());
        header[6..8].copy_from_slice(&self.config.sample_format.to_wire().to_le_bytes());
        header[8..12].copy_from_slice(&self.frame_count.to_le_bytes());
        header[12..20].copy_from_slice(&self.timestamp_us.to_le_bytes());
        header[20..24].copy_from_slice(&(self.data.len() as u32).to_le_bytes());
        header
    }

    /// Decode a frame header from iroh wire protocol (24 bytes).
    pub fn decode_header(header: &[u8; WIRE_HEADER_SIZE]) -> Result<(AudioConfig, u32, u64, u32)> {
        let sample_rate = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        let channels = u16::from_le_bytes([header[4], header[5]]);
        let sample_format = SampleFormat::from_wire(u16::from_le_bytes([header[6], header[7]]))?;
        let frame_count = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
        let timestamp_us = u64::from_le_bytes([
            header[12], header[13], header[14], header[15], header[16], header[17], header[18],
            header[19],
        ]);
        let data_length = u32::from_le_bytes([header[20], header[21], header[22], header[23]]);

        Ok((
            AudioConfig {
                sample_rate,
                channels,
                sample_format,
            },
            frame_count,
            timestamp_us,
            data_length,
        ))
    }

    /// Encode the frame for MoQ (20-byte header + data, self-delimiting).
    pub fn encode_moq(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(MOQ_HEADER_SIZE + self.data.len());
        buf.extend_from_slice(&self.config.sample_rate.to_le_bytes());
        buf.extend_from_slice(&self.config.channels.to_le_bytes());
        buf.extend_from_slice(&self.config.sample_format.to_wire().to_le_bytes());
        buf.extend_from_slice(&self.frame_count.to_le_bytes());
        buf.extend_from_slice(&(self.timestamp_us as u32).to_le_bytes()); // truncated
        buf.extend_from_slice(&self.data.len().to_le_bytes()[..4]);
        buf.extend_from_slice(&self.data);
        buf
    }

    /// Decode a MoQ frame (20-byte header + data).
    pub fn decode_moq(data: &[u8]) -> Result<Self> {
        if data.len() < MOQ_HEADER_SIZE {
            anyhow::bail!("MoQ audio frame too short: {} bytes", data.len());
        }
        let sample_rate = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let channels = u16::from_le_bytes([data[4], data[5]]);
        let sample_format = SampleFormat::from_wire(u16::from_le_bytes([data[6], data[7]]))?;
        let frame_count = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        let timestamp_us = u32::from_le_bytes([data[12], data[13], data[14], data[15]]) as u64;
        let data_length = u32::from_le_bytes([data[16], data[17], data[18], data[19]]) as usize;
        let pcm_data = data[MOQ_HEADER_SIZE..MOQ_HEADER_SIZE + data_length].to_vec();

        Ok(AudioFrame {
            data: pcm_data,
            frame_count,
            timestamp_us,
            config: AudioConfig {
                sample_rate,
                channels,
                sample_format,
            },
        })
    }
}

/// Information about an audio device.
#[derive(Debug, Clone)]
pub struct AudioDeviceInfo {
    pub index: usize,
    pub name: String,
}

/// Audio device enumeration.
pub struct AudioDevice;

impl AudioDevice {
    /// List available input (microphone) devices.
    pub fn list_inputs() -> Result<Vec<AudioDeviceInfo>> {
        let host = cpal::default_host();
        let devices: Vec<_> = host
            .input_devices()
            .map_err(|e| anyhow::anyhow!("Failed to enumerate input devices: {}", e))?
            .enumerate()
            .map(|(i, d)| AudioDeviceInfo {
                index: i,
                name: d.name().unwrap_or_else(|_| format!("Input {}", i)),
            })
            .collect();
        Ok(devices)
    }

    /// List available output (speaker) devices.
    pub fn list_outputs() -> Result<Vec<AudioDeviceInfo>> {
        let host = cpal::default_host();
        let devices: Vec<_> = host
            .output_devices()
            .map_err(|e| anyhow::anyhow!("Failed to enumerate output devices: {}", e))?
            .enumerate()
            .map(|(i, d)| AudioDeviceInfo {
                index: i,
                name: d.name().unwrap_or_else(|_| format!("Output {}", i)),
            })
            .collect();
        Ok(devices)
    }

    /// Get the default input device name.
    pub fn default_input() -> Result<AudioDeviceInfo> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| anyhow::anyhow!("No default input device"))?;
        Ok(AudioDeviceInfo {
            index: 0,
            name: device
                .name()
                .unwrap_or_else(|_| "Default Input".to_string()),
        })
    }

    /// Get the default output device name.
    pub fn default_output() -> Result<AudioDeviceInfo> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow::anyhow!("No default output device"))?;
        Ok(AudioDeviceInfo {
            index: 0,
            name: device
                .name()
                .unwrap_or_else(|_| "Default Output".to_string()),
        })
    }
}

/// Captures audio from a microphone.
///
/// Wraps a cpal input stream. Audio data is captured in a callback
/// and sent to a channel, which `read()` pulls from.
pub struct AudioInput {
    rx: mpsc::Receiver<AudioFrame>,
    _stream: cpal::Stream,
    config: AudioConfig,
}

impl AudioInput {
    /// Open the default input device with the given config.
    pub fn open(config: AudioConfig) -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| anyhow::anyhow!("No default input device"))?;
        Self::open_device(&device, config)
    }

    /// Open a specific input device by index.
    pub fn open_index(index: usize, config: AudioConfig) -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .input_devices()
            .map_err(|e| anyhow::anyhow!("Failed to enumerate input devices: {}", e))?
            .nth(index)
            .ok_or_else(|| anyhow::anyhow!("Input device {} not found", index))?;
        Self::open_device(&device, config)
    }

    /// Open a specific input device by name substring match.
    pub fn open_name(name: &str, config: AudioConfig) -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .input_devices()
            .map_err(|e| anyhow::anyhow!("Failed to enumerate input devices: {}", e))?
            .find(|d| d.name().map(|n| n.contains(name)).unwrap_or(false))
            .ok_or_else(|| anyhow::anyhow!("No input device matching '{}'", name))?;
        let dev_name = device.name().unwrap_or_default();
        tracing::info!("Opened input device: {}", dev_name);
        Self::open_device(&device, config)
    }

    fn open_device(device: &cpal::Device, config: AudioConfig) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<AudioFrame>();

        let stream_config = cpal::StreamConfig {
            channels: config.channels,
            sample_rate: cpal::SampleRate(config.sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        let cfg = config.clone();
        let stream = match config.sample_format {
            SampleFormat::I16 => device.build_input_stream(
                &stream_config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    let bytes: Vec<u8> = data.iter().flat_map(|s| s.to_le_bytes()).collect();
                    let timestamp_us = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_micros() as u64;
                    let _ = tx.send(AudioFrame {
                        data: bytes,
                        frame_count: (data.len() / cfg.channels as usize) as u32,
                        timestamp_us,
                        config: cfg.clone(),
                    });
                },
                |err| tracing::error!("Audio input error: {}", err),
                None,
            )?,
            SampleFormat::F32 => device.build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let bytes: Vec<u8> = data.iter().flat_map(|s| s.to_le_bytes()).collect();
                    let timestamp_us = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_micros() as u64;
                    let _ = tx.send(AudioFrame {
                        data: bytes,
                        frame_count: (data.len() / cfg.channels as usize) as u32,
                        timestamp_us,
                        config: cfg.clone(),
                    });
                },
                |err| tracing::error!("Audio input error: {}", err),
                None,
            )?,
        };

        stream.play()?;

        Ok(AudioInput {
            rx,
            _stream: stream,
            config,
        })
    }

    /// Read the next audio frame (blocks until data is available).
    pub fn read(&self) -> Result<AudioFrame> {
        self.rx
            .recv()
            .map_err(|_| anyhow::anyhow!("Audio input stream closed"))
    }

    /// Try to read an audio frame without blocking.
    pub fn try_read(&self) -> Option<AudioFrame> {
        self.rx.try_recv().ok()
    }

    /// Get the audio config.
    pub fn config(&self) -> &AudioConfig {
        &self.config
    }
}

/// Plays audio to a speaker.
///
/// Wraps a cpal output stream. Audio data is pushed into a ring buffer
/// which the cpal callback drains.
pub struct AudioOutput {
    tx: mpsc::SyncSender<Vec<u8>>,
    _stream: cpal::Stream,
    config: AudioConfig,
}

impl AudioOutput {
    /// Open the default output device with the given config.
    pub fn open(config: AudioConfig) -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow::anyhow!("No default output device"))?;
        Self::open_device(&device, config)
    }

    /// Open a specific output device by index.
    pub fn open_index(index: usize, config: AudioConfig) -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .output_devices()
            .map_err(|e| anyhow::anyhow!("Failed to enumerate output devices: {}", e))?
            .nth(index)
            .ok_or_else(|| anyhow::anyhow!("Output device {} not found", index))?;
        Self::open_device(&device, config)
    }

    fn open_device(device: &cpal::Device, config: AudioConfig) -> Result<Self> {
        // Buffer up to 10 chunks (~200ms at 20ms chunks)
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(10);

        let stream_config = cpal::StreamConfig {
            channels: config.channels,
            sample_rate: cpal::SampleRate(config.sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        let stream = match config.sample_format {
            SampleFormat::I16 => {
                let rx = std::sync::Mutex::new(rx);
                let mut pending: Vec<u8> = Vec::new();
                device.build_output_stream(
                    &stream_config,
                    move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                        let needed_bytes = data.len() * 2;
                        // Fill from pending buffer and channel
                        while pending.len() < needed_bytes {
                            match rx.lock().unwrap().try_recv() {
                                Ok(chunk) => pending.extend(chunk),
                                Err(_) => break,
                            }
                        }
                        if pending.len() >= needed_bytes {
                            for (i, sample) in data.iter_mut().enumerate() {
                                let offset = i * 2;
                                *sample =
                                    i16::from_le_bytes([pending[offset], pending[offset + 1]]);
                            }
                            pending.drain(..needed_bytes);
                        } else {
                            // Underrun — output silence
                            for sample in data.iter_mut() {
                                *sample = 0;
                            }
                            pending.clear();
                        }
                    },
                    |err| tracing::error!("Audio output error: {}", err),
                    None,
                )?
            }
            SampleFormat::F32 => {
                let rx = std::sync::Mutex::new(rx);
                let mut pending: Vec<u8> = Vec::new();
                device.build_output_stream(
                    &stream_config,
                    move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                        let needed_bytes = data.len() * 4;
                        while pending.len() < needed_bytes {
                            match rx.lock().unwrap().try_recv() {
                                Ok(chunk) => pending.extend(chunk),
                                Err(_) => break,
                            }
                        }
                        if pending.len() >= needed_bytes {
                            for (i, sample) in data.iter_mut().enumerate() {
                                let offset = i * 4;
                                *sample = f32::from_le_bytes([
                                    pending[offset],
                                    pending[offset + 1],
                                    pending[offset + 2],
                                    pending[offset + 3],
                                ]);
                            }
                            pending.drain(..needed_bytes);
                        } else {
                            for sample in data.iter_mut() {
                                *sample = 0.0;
                            }
                            pending.clear();
                        }
                    },
                    |err| tracing::error!("Audio output error: {}", err),
                    None,
                )?
            }
        };

        stream.play()?;

        Ok(AudioOutput {
            tx,
            _stream: stream,
            config,
        })
    }

    /// Write an audio frame to the output.
    pub fn write(&self, frame: &AudioFrame) -> Result<()> {
        self.tx
            .send(frame.data.clone())
            .map_err(|_| anyhow::anyhow!("Audio output stream closed"))
    }

    /// Write raw PCM bytes to the output.
    pub fn write_raw(&self, data: Vec<u8>) -> Result<()> {
        self.tx
            .send(data)
            .map_err(|_| anyhow::anyhow!("Audio output stream closed"))
    }

    /// Get the audio config.
    pub fn config(&self) -> &AudioConfig {
        &self.config
    }
}

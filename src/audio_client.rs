//! Sync audio client for Python bindings.
//!
//! This module provides a blocking API for remote audio,
//! managing its own tokio runtime internally.

use anyhow::Result;

use crate::audio::{AudioConfig, AudioFrame};
use crate::sounddevice_impl::AudioStreamBuilder;

/// A synchronous client for remote audio streams.
///
/// This client wraps the blocking `RemoteAudioStream`,
/// providing a simple API for Python bindings.
pub struct SyncAudioClient {
    inner: crate::sounddevice_impl::RemoteAudioStream,
    config: AudioConfig,
}

impl SyncAudioClient {
    /// Connect to a remote audio server via iroh P2P.
    pub fn connect(server_id: &str) -> Result<Self> {
        Self::connect_with_config(server_id, AudioConfig::default())
    }

    /// Connect with custom audio config.
    pub fn connect_with_config(server_id: &str, config: AudioConfig) -> Result<Self> {
        let stream = AudioStreamBuilder::new(server_id)
            .sample_rate(config.sample_rate)
            .channels(config.channels)
            .sample_format(config.sample_format)
            .open()?;

        Ok(Self {
            inner: stream,
            config,
        })
    }

    /// Connect to a remote audio server via MoQ relay.
    pub fn connect_moq(path: &str) -> Result<Self> {
        Self::connect_moq_with_config(path, AudioConfig::default())
    }

    /// Connect via MoQ with custom config.
    pub fn connect_moq_with_config(path: &str, config: AudioConfig) -> Result<Self> {
        let stream = AudioStreamBuilder::new(path)
            .sample_rate(config.sample_rate)
            .channels(config.channels)
            .sample_format(config.sample_format)
            .open()?;

        Ok(Self {
            inner: stream,
            config,
        })
    }

    /// Auto-detect transport and connect.
    ///
    /// Uses MoQ if the source contains `/` (e.g. `anon/audio-0`),
    /// otherwise treats it as an iroh server ID.
    pub fn connect_auto(source: &str) -> Result<Self> {
        if source.contains('/') {
            Self::connect_moq(source)
        } else {
            Self::connect(source)
        }
    }

    /// Read an audio frame from the remote microphone.
    pub fn read_chunk(&mut self) -> Result<AudioFrame> {
        self.inner.read_chunk()
    }

    /// Write an audio frame to the remote speaker.
    pub fn write_chunk(&mut self, frame: &AudioFrame) -> Result<()> {
        self.inner.write_chunk(frame)
    }

    /// Record audio for the given number of frames, returning f32 samples.
    pub fn record(&mut self, frames: usize) -> Result<Vec<f32>> {
        self.inner.record(frames)
    }

    /// Play f32 audio samples to the remote speaker.
    pub fn play(&mut self, data: &[f32]) -> Result<()> {
        self.inner.play(data)
    }

    /// Get the audio config.
    pub fn config(&self) -> &AudioConfig {
        &self.config
    }
}

// PyO3 generated code triggers this lint on #[pymethods] error conversions
#![allow(clippy::useless_conversion)]

//! Drop-in replacement for sounddevice - remote audio over P2P.
//!
//! This module provides a `sounddevice`-compatible API that connects
//! to remote audio devices over iroh P2P or MoQ relay.
//!
//! # Example
//!
//! ```python
//! import xoq_sounddevice as sd
//!
//! # Bidirectional stream
//! stream = sd.Stream("server-id", samplerate=48000, channels=1)
//! data = stream.read(960)     # Read 960 frames (20ms @ 48kHz)
//! stream.write(data)          # Send audio to remote speaker
//!
//! # Record convenience function
//! data = sd.rec("server-id", frames=48000, samplerate=48000, channels=1)
//! ```

use numpy::PyArrayMethods;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use std::sync::{Arc, Mutex};

/// Bidirectional audio stream to a remote audio server.
///
/// Provides `read()` and `write()` for duplex audio communication.
///
/// Example:
///     stream = Stream("server-endpoint-id", samplerate=48000, channels=1)
///     data = stream.read(960)    # numpy array (960, 1) float32
///     stream.write(data)
///     stream.stop()
#[pyclass]
pub struct Stream {
    inner: Arc<Mutex<Option<xoq::SyncAudioClient>>>,
    is_open: Arc<std::sync::atomic::AtomicBool>,
    samplerate: u32,
    channels: u16,
    blocksize: u32,
}

#[pymethods]
impl Stream {
    /// Open a bidirectional audio stream to a remote server.
    ///
    /// Args:
    ///     source: Server endpoint ID (iroh) or MoQ path (e.g. "anon/audio-0")
    ///     samplerate: Sample rate in Hz (default: 48000)
    ///     channels: Number of channels (default: 1)
    ///     blocksize: Default block size in frames (default: 960 = 20ms @ 48kHz)
    #[new]
    #[pyo3(signature = (source, samplerate=48000, channels=1, blocksize=960))]
    fn new(source: &str, samplerate: u32, channels: u16, blocksize: u32) -> PyResult<Self> {
        let config = xoq::audio::AudioConfig {
            sample_rate: samplerate,
            channels,
            sample_format: xoq::audio::SampleFormat::I16,
        };

        let client = if source.contains('/') {
            xoq::SyncAudioClient::connect_moq_with_config(source, config)
        } else {
            xoq::SyncAudioClient::connect_with_config(source, config)
        }
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

        Ok(Stream {
            inner: Arc::new(Mutex::new(Some(client))),
            is_open: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            samplerate,
            channels,
            blocksize,
        })
    }

    /// Read audio frames from the remote microphone.
    ///
    /// Args:
    ///     frames: Number of frames to read (default: blocksize)
    ///
    /// Returns:
    ///     numpy array of shape (frames, channels), dtype float32
    #[pyo3(signature = (frames=None))]
    fn read<'py>(&self, py: Python<'py>, frames: Option<u32>) -> PyResult<PyObject> {
        let frames = frames.unwrap_or(self.blocksize) as usize;
        let channels = self.channels as usize;

        if !self.is_open.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(PyRuntimeError::new_err("Stream is closed"));
        }

        // Take client out so we can release the GIL
        let mut client = {
            let mut guard = self
                .inner
                .lock()
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            guard.take()
        };

        let result = if let Some(ref mut c) = client {
            py.allow_threads(|| c.record(frames))
        } else {
            Err(anyhow::anyhow!("Stream not connected"))
        };

        // Put client back
        if let Some(c) = client {
            let mut guard = self
                .inner
                .lock()
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            *guard = Some(c);
        }

        match result {
            Ok(samples) => {
                let array = numpy::PyArray1::from_vec_bound(py, samples);
                let reshaped = array
                    .reshape([frames, channels])
                    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
                Ok(reshaped.into_any().unbind())
            }
            Err(e) => Err(PyRuntimeError::new_err(e.to_string())),
        }
    }

    /// Write audio frames to the remote speaker.
    ///
    /// Args:
    ///     data: numpy array of shape (frames, channels) or (frames,), dtype float32
    fn write(&self, py: Python<'_>, data: &Bound<'_, numpy::PyArray1<f32>>) -> PyResult<()> {
        if !self.is_open.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(PyRuntimeError::new_err("Stream is closed"));
        }

        // Read data from numpy array
        let samples: Vec<f32> = data
            .to_vec()
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

        // Take client out so we can release the GIL
        let mut client = {
            let mut guard = self
                .inner
                .lock()
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            guard.take()
        };

        let result = if let Some(ref mut c) = client {
            py.allow_threads(|| c.play(&samples))
        } else {
            Err(anyhow::anyhow!("Stream not connected"))
        };

        // Put client back
        if let Some(c) = client {
            let mut guard = self
                .inner
                .lock()
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            *guard = Some(c);
        }

        result.map_err(|e| PyRuntimeError::new_err(e.to_string()))
    }

    /// Start the stream (no-op, stream starts on creation).
    fn start(&self) -> PyResult<()> {
        Ok(())
    }

    /// Stop the stream.
    fn stop(&self) {
        self.is_open
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }

    /// Close and release the stream.
    fn close(&self) {
        self.is_open
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut guard) = self.inner.lock() {
            *guard = None;
        }
    }

    /// Get the sample rate.
    #[getter]
    fn samplerate(&self) -> u32 {
        self.samplerate
    }

    /// Get the number of channels.
    #[getter]
    fn channels(&self) -> u16 {
        self.channels
    }

    /// Get the block size.
    #[getter]
    fn blocksize(&self) -> u32 {
        self.blocksize
    }

    /// Check if the stream is active.
    #[getter]
    fn active(&self) -> bool {
        self.is_open.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Input-only audio stream (reads from remote microphone).
///
/// Example:
///     stream = InputStream("server-id", samplerate=48000, channels=1)
///     data = stream.read(960)
#[pyclass]
pub struct InputStream {
    stream: Stream,
}

#[pymethods]
impl InputStream {
    #[new]
    #[pyo3(signature = (source, samplerate=48000, channels=1, blocksize=960))]
    fn new(source: &str, samplerate: u32, channels: u16, blocksize: u32) -> PyResult<Self> {
        Ok(InputStream {
            stream: Stream::new(source, samplerate, channels, blocksize)?,
        })
    }

    #[pyo3(signature = (frames=None))]
    fn read<'py>(&self, py: Python<'py>, frames: Option<u32>) -> PyResult<PyObject> {
        self.stream.read(py, frames)
    }

    fn start(&self) -> PyResult<()> {
        self.stream.start()
    }

    fn stop(&self) {
        self.stream.stop();
    }

    fn close(&self) {
        self.stream.close();
    }

    #[getter]
    fn samplerate(&self) -> u32 {
        self.stream.samplerate
    }

    #[getter]
    fn channels(&self) -> u16 {
        self.stream.channels
    }

    #[getter]
    fn active(&self) -> bool {
        self.stream
            .is_open
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Output-only audio stream (writes to remote speaker).
///
/// Example:
///     stream = OutputStream("server-id", samplerate=48000, channels=1)
///     stream.write(data)
#[pyclass]
pub struct OutputStream {
    stream: Stream,
}

#[pymethods]
impl OutputStream {
    #[new]
    #[pyo3(signature = (source, samplerate=48000, channels=1, blocksize=960))]
    fn new(source: &str, samplerate: u32, channels: u16, blocksize: u32) -> PyResult<Self> {
        Ok(OutputStream {
            stream: Stream::new(source, samplerate, channels, blocksize)?,
        })
    }

    fn write(&self, py: Python<'_>, data: &Bound<'_, numpy::PyArray1<f32>>) -> PyResult<()> {
        self.stream.write(py, data)
    }

    fn start(&self) -> PyResult<()> {
        self.stream.start()
    }

    fn stop(&self) {
        self.stream.stop();
    }

    fn close(&self) {
        self.stream.close();
    }

    #[getter]
    fn samplerate(&self) -> u32 {
        self.stream.samplerate
    }

    #[getter]
    fn channels(&self) -> u16 {
        self.stream.channels
    }

    #[getter]
    fn active(&self) -> bool {
        self.stream
            .is_open
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Record audio from a remote microphone.
///
/// Args:
///     source: Server endpoint ID or MoQ path
///     frames: Number of frames to record
///     samplerate: Sample rate in Hz (default: 48000)
///     channels: Number of channels (default: 1)
///
/// Returns:
///     numpy array of shape (frames, channels), dtype float32
#[pyfunction]
#[pyo3(signature = (source, frames, samplerate=48000, channels=1))]
fn rec<'py>(
    py: Python<'py>,
    source: &str,
    frames: usize,
    samplerate: u32,
    channels: u16,
) -> PyResult<PyObject> {
    let config = xoq::audio::AudioConfig {
        sample_rate: samplerate,
        channels,
        sample_format: xoq::audio::SampleFormat::I16,
    };

    let result = py.allow_threads(|| {
        let mut client = xoq::SyncAudioClient::connect_with_config(source, config)?;
        client.record(frames)
    });

    match result {
        Ok(samples) => {
            let array = numpy::PyArray1::from_vec_bound(py, samples);
            let reshaped = array
                .reshape([frames, channels as usize])
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            Ok(reshaped.into_any().unbind())
        }
        Err(e) => Err(PyRuntimeError::new_err(e.to_string())),
    }
}

/// Play audio to a remote speaker.
///
/// Args:
///     source: Server endpoint ID or MoQ path
///     data: numpy array, dtype float32
///     samplerate: Sample rate in Hz (default: 48000)
#[pyfunction]
#[pyo3(signature = (source, data, samplerate=48000))]
fn play(
    py: Python<'_>,
    source: &str,
    data: &Bound<'_, numpy::PyArray1<f32>>,
    samplerate: u32,
) -> PyResult<()> {
    let samples: Vec<f32> = data
        .to_vec()
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

    let config = xoq::audio::AudioConfig {
        sample_rate: samplerate,
        channels: 1,
        sample_format: xoq::audio::SampleFormat::I16,
    };

    py.allow_threads(|| {
        let mut client = xoq::SyncAudioClient::connect_with_config(source, config)?;
        client.play(&samples)
    })
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))
}

#[pymodule]
fn xoq_sounddevice(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Stream>()?;
    m.add_class::<InputStream>()?;
    m.add_class::<OutputStream>()?;
    m.add_function(wrap_pyfunction!(rec, m)?)?;
    m.add_function(wrap_pyfunction!(play, m)?)?;
    Ok(())
}

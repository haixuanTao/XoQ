//! macOS native audio backend using Voice Processing IO (VPIO).
//!
//! Uses Apple's `kAudioUnitSubType_VoiceProcessingIO` AudioUnit which provides
//! built-in acoustic echo cancellation (AEC), noise suppression, and automatic
//! gain control (AGC) — the same processing pipeline as FaceTime.
//!
//! Unlike cpal where input and output are separate streams, VPIO uses a single
//! AudioUnit with two buses:
//! - **Bus 0 (output)**: Speaker playback — also serves as AEC reference signal
//! - **Bus 1 (input)**: Mic capture — AEC applied using bus 0 correlation
//!
//! They must be in the same AudioUnit for AEC to work.
//!
//! # Example
//!
//! ```rust,no_run
//! use xoq::audio::{AudioConfig, SampleFormat};
//! use xoq::audio_macos::AudioVoiceIO;
//!
//! let config = AudioConfig {
//!     sample_rate: 48000,
//!     channels: 1,
//!     sample_format: SampleFormat::I16,
//! };
//!
//! let vpio = AudioVoiceIO::open(config).unwrap();
//!
//! // Read mic input (with AEC applied)
//! let frame = vpio.read().unwrap();
//!
//! // Write to speaker (serves as AEC reference)
//! vpio.write(&frame).unwrap();
//! ```

use anyhow::Result;
use std::ffi::c_void;
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::audio::{AudioConfig, AudioFrame, SampleFormat};

// =============================================================================
// CoreAudio FFI declarations
// =============================================================================

#[link(name = "AudioToolbox", kind = "framework")]
extern "C" {
    fn AudioComponentFindNext(
        in_component: *mut c_void,
        in_desc: *const AudioComponentDescription,
    ) -> *mut c_void;

    fn AudioComponentInstanceNew(in_component: *mut c_void, out_instance: *mut *mut c_void) -> i32;

    fn AudioComponentInstanceDispose(in_instance: *mut c_void) -> i32;

    fn AudioUnitSetProperty(
        in_unit: *mut c_void,
        in_id: u32,
        in_scope: u32,
        in_element: u32,
        in_data: *const c_void,
        in_data_size: u32,
    ) -> i32;

    fn AudioUnitInitialize(in_unit: *mut c_void) -> i32;

    fn AudioUnitUninitialize(in_unit: *mut c_void) -> i32;

    fn AudioOutputUnitStart(ci: *mut c_void) -> i32;

    fn AudioOutputUnitStop(ci: *mut c_void) -> i32;

    fn AudioUnitRender(
        in_unit: *mut c_void,
        io_action_flags: *mut u32,
        in_time_stamp: *const AudioTimeStamp,
        in_output_bus_number: u32,
        in_number_frames: u32,
        io_data: *mut AudioBufferList,
    ) -> i32;
}

// =============================================================================
// CoreAudio types (repr(C))
// =============================================================================

#[repr(C)]
struct AudioComponentDescription {
    component_type: u32,
    component_sub_type: u32,
    component_manufacturer: u32,
    component_flags: u32,
    component_flags_mask: u32,
}

#[repr(C)]
#[derive(Clone)]
struct AudioStreamBasicDescription {
    sample_rate: f64,
    format_id: u32,
    format_flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels_per_frame: u32,
    bits_per_channel: u32,
    reserved: u32,
}

#[repr(C)]
struct AudioBuffer {
    number_channels: u32,
    data_byte_size: u32,
    data: *mut c_void,
}

// AudioBufferList with 1 buffer (most common case for mono/interleaved)
#[repr(C)]
struct AudioBufferList {
    number_buffers: u32,
    buffers: [AudioBuffer; 1],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioTimeStamp {
    sample_time: f64,
    host_time: u64,
    rate_scalar: f64,
    word_clock_time: u64,
    smpte_time: SMPTETime,
    flags: u32,
    reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SMPTETime {
    subframes: i16,
    subframe_divisor: i16,
    counter: u32,
    smpte_type: u32,
    flags: u32,
    hours: i16,
    minutes: i16,
    seconds: i16,
    frames: i16,
}

#[repr(C)]
struct AURenderCallbackStruct {
    input_proc: unsafe extern "C" fn(
        in_ref_con: *mut c_void,
        io_action_flags: *mut u32,
        in_time_stamp: *const AudioTimeStamp,
        in_bus_number: u32,
        in_number_frames: u32,
        io_data: *mut AudioBufferList,
    ) -> i32,
    input_proc_ref_con: *mut c_void,
}

// =============================================================================
// Constants
// =============================================================================

const K_AUDIO_UNIT_TYPE_OUTPUT: u32 = 0x6175_6F75; // 'auou'
const K_AUDIO_UNIT_SUB_TYPE_VOICE_PROCESSING_IO: u32 = 0x7670_696F; // 'vpio'
const K_AUDIO_UNIT_MANUFACTURER_APPLE: u32 = 0x6170_706C; // 'appl'

// Property IDs
const K_AUDIO_OUTPUT_UNIT_PROPERTY_ENABLE_IO: u32 = 2003;
const K_AUDIO_UNIT_PROPERTY_STREAM_FORMAT: u32 = 8;
const K_AUDIO_UNIT_PROPERTY_SET_RENDER_CALLBACK: u32 = 23;
const K_AUDIO_OUTPUT_UNIT_PROPERTY_SET_INPUT_CALLBACK: u32 = 2005;

// Scope
const K_AUDIO_UNIT_SCOPE_INPUT: u32 = 1;
const K_AUDIO_UNIT_SCOPE_OUTPUT: u32 = 2;
const K_AUDIO_UNIT_SCOPE_GLOBAL: u32 = 0;

// Format
const K_AUDIO_FORMAT_LINEAR_PCM: u32 = 0x6C70_636D; // 'lpcm'
const K_AUDIO_FORMAT_FLAG_IS_SIGNED_INTEGER: u32 = 1 << 2;
const K_AUDIO_FORMAT_FLAG_IS_PACKED: u32 = 1 << 3;
const K_AUDIO_FORMAT_FLAG_IS_FLOAT: u32 = 1 << 0;

// =============================================================================
// Callback state
// =============================================================================

struct CallbackState {
    /// Sends captured mic frames (from input callback) to read()
    input_tx: mpsc::SyncSender<AudioFrame>,
    /// Receives speaker data (from write()) for output callback
    output_rx: std::sync::Mutex<mpsc::Receiver<Vec<u8>>>,
    /// AudioUnit pointer (needed for AudioUnitRender in input callback)
    unit: *mut c_void,
    /// Audio config for building AudioFrames
    config: AudioConfig,
}

// Safety: The CallbackState is heap-allocated and accessed only from CoreAudio
// callbacks and the AudioVoiceIO struct. The AudioUnit pointer is valid for the
// lifetime of the AudioVoiceIO.
unsafe impl Send for CallbackState {}
unsafe impl Sync for CallbackState {}

// =============================================================================
// Input callback (mic capture)
// =============================================================================

unsafe extern "C" fn input_callback(
    in_ref_con: *mut c_void,
    io_action_flags: *mut u32,
    in_time_stamp: *const AudioTimeStamp,
    _in_bus_number: u32,
    in_number_frames: u32,
    _io_data: *mut AudioBufferList,
) -> i32 {
    let state = &*(in_ref_con as *const CallbackState);

    let bytes_per_sample = state.config.sample_format.bytes_per_sample();
    let buf_size = in_number_frames as usize * state.config.channels as usize * bytes_per_sample;
    let mut buffer = vec![0u8; buf_size];

    let mut abl = AudioBufferList {
        number_buffers: 1,
        buffers: [AudioBuffer {
            number_channels: state.config.channels as u32,
            data_byte_size: buf_size as u32,
            data: buffer.as_mut_ptr() as *mut c_void,
        }],
    };

    // Pull mic data from bus 1
    let status = AudioUnitRender(
        state.unit,
        io_action_flags,
        in_time_stamp,
        1, // bus 1 = input
        in_number_frames,
        &mut abl,
    );

    if status != 0 {
        return status;
    }

    let actual_size = abl.buffers[0].data_byte_size as usize;
    buffer.truncate(actual_size);

    let timestamp_us = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;

    let frame = AudioFrame {
        data: buffer,
        frame_count: in_number_frames,
        timestamp_us,
        config: state.config.clone(),
    };

    // Non-blocking send — drop frame if channel is full
    let _ = state.input_tx.try_send(frame);

    0 // noErr
}

// =============================================================================
// Output callback (speaker playback)
// =============================================================================

unsafe extern "C" fn output_callback(
    in_ref_con: *mut c_void,
    _io_action_flags: *mut u32,
    _in_time_stamp: *const AudioTimeStamp,
    _in_bus_number: u32,
    in_number_frames: u32,
    io_data: *mut AudioBufferList,
) -> i32 {
    let state = &*(in_ref_con as *const CallbackState);

    let bytes_per_sample = state.config.sample_format.bytes_per_sample();
    let needed = in_number_frames as usize * state.config.channels as usize * bytes_per_sample;

    let abl = &mut *io_data;
    let out_buf = std::slice::from_raw_parts_mut(
        abl.buffers[0].data as *mut u8,
        abl.buffers[0].data_byte_size as usize,
    );

    let mut filled = 0usize;

    if let Ok(rx) = state.output_rx.lock() {
        while filled < needed {
            match rx.try_recv() {
                Ok(chunk) => {
                    let to_copy = chunk.len().min(needed - filled);
                    out_buf[filled..filled + to_copy].copy_from_slice(&chunk[..to_copy]);
                    filled += to_copy;
                }
                Err(_) => break,
            }
        }
    }

    // Fill remainder with silence
    if filled < needed {
        out_buf[filled..needed].fill(0);
    }

    0 // noErr
}

// =============================================================================
// AudioVoiceIO public API
// =============================================================================

/// Bidirectional audio using macOS Voice Processing IO.
///
/// Provides mic capture with AEC/noise suppression and speaker playback
/// through a single AudioUnit. AEC uses the speaker output as reference
/// to cancel echo from the mic input.
pub struct AudioVoiceIO {
    unit: *mut c_void,
    input_rx: mpsc::Receiver<AudioFrame>,
    output_tx: mpsc::SyncSender<Vec<u8>>,
    config: AudioConfig,
    // Prevent CallbackState from being dropped while AudioUnit is alive
    _callback_state: *mut CallbackState,
}

// Safety: AudioUnit operations are thread-safe when properly initialized.
// The CallbackState is pinned on the heap and outlives all callbacks.
unsafe impl Send for AudioVoiceIO {}

impl AudioVoiceIO {
    /// Open a Voice Processing IO AudioUnit with the given config.
    ///
    /// This creates a bidirectional audio unit with AEC, noise suppression,
    /// and AGC enabled. Both mic and speaker start immediately.
    pub fn open(config: AudioConfig) -> Result<Self> {
        unsafe { Self::open_inner(config) }
    }

    unsafe fn open_inner(config: AudioConfig) -> Result<Self> {
        // 1. Find the VPIO AudioComponent
        let desc = AudioComponentDescription {
            component_type: K_AUDIO_UNIT_TYPE_OUTPUT,
            component_sub_type: K_AUDIO_UNIT_SUB_TYPE_VOICE_PROCESSING_IO,
            component_manufacturer: K_AUDIO_UNIT_MANUFACTURER_APPLE,
            component_flags: 0,
            component_flags_mask: 0,
        };

        let component = AudioComponentFindNext(std::ptr::null_mut(), &desc);
        if component.is_null() {
            anyhow::bail!("VoiceProcessingIO AudioUnit not found");
        }

        // 2. Create AudioUnit instance
        let mut unit: *mut c_void = std::ptr::null_mut();
        let status = AudioComponentInstanceNew(component, &mut unit);
        if status != 0 {
            anyhow::bail!(
                "AudioComponentInstanceNew failed: {}",
                osstatus_description(status)
            );
        }

        // 3. Enable input on bus 1
        let enable: u32 = 1;
        let status = AudioUnitSetProperty(
            unit,
            K_AUDIO_OUTPUT_UNIT_PROPERTY_ENABLE_IO,
            K_AUDIO_UNIT_SCOPE_INPUT,
            1, // bus 1 = input (mic)
            &enable as *const u32 as *const c_void,
            std::mem::size_of::<u32>() as u32,
        );
        if status != 0 {
            AudioComponentInstanceDispose(unit);
            anyhow::bail!(
                "Failed to enable input on bus 1: {}",
                osstatus_description(status)
            );
        }

        // 4. Set stream format on bus 0 input scope (speaker format)
        let asbd = config_to_asbd(&config);

        let status = AudioUnitSetProperty(
            unit,
            K_AUDIO_UNIT_PROPERTY_STREAM_FORMAT,
            K_AUDIO_UNIT_SCOPE_INPUT,
            0, // bus 0 = output (speaker)
            &asbd as *const AudioStreamBasicDescription as *const c_void,
            std::mem::size_of::<AudioStreamBasicDescription>() as u32,
        );
        if status != 0 {
            AudioComponentInstanceDispose(unit);
            anyhow::bail!(
                "Failed to set output stream format: {}",
                osstatus_description(status)
            );
        }

        // 5. Set stream format on bus 1 output scope (mic format)
        let status = AudioUnitSetProperty(
            unit,
            K_AUDIO_UNIT_PROPERTY_STREAM_FORMAT,
            K_AUDIO_UNIT_SCOPE_OUTPUT,
            1, // bus 1 = input (mic)
            &asbd as *const AudioStreamBasicDescription as *const c_void,
            std::mem::size_of::<AudioStreamBasicDescription>() as u32,
        );
        if status != 0 {
            AudioComponentInstanceDispose(unit);
            anyhow::bail!(
                "Failed to set input stream format: {}",
                osstatus_description(status)
            );
        }

        // 6. Create channels and callback state
        // Input: buffer up to 64 frames (~1.3s at 48kHz/1024 frames)
        let (input_tx, input_rx) = mpsc::sync_channel::<AudioFrame>(64);
        // Output: buffer up to 10 chunks (~200ms at 20ms chunks)
        let (output_tx, output_rx) = mpsc::sync_channel::<Vec<u8>>(10);

        let callback_state = Box::into_raw(Box::new(CallbackState {
            input_tx,
            output_rx: std::sync::Mutex::new(output_rx),
            unit,
            config: config.clone(),
        }));

        // 7. Set render callback on bus 0 (speaker data provider)
        let render_cb = AURenderCallbackStruct {
            input_proc: output_callback,
            input_proc_ref_con: callback_state as *mut c_void,
        };

        let status = AudioUnitSetProperty(
            unit,
            K_AUDIO_UNIT_PROPERTY_SET_RENDER_CALLBACK,
            K_AUDIO_UNIT_SCOPE_GLOBAL,
            0, // bus 0 = output
            &render_cb as *const AURenderCallbackStruct as *const c_void,
            std::mem::size_of::<AURenderCallbackStruct>() as u32,
        );
        if status != 0 {
            let _ = Box::from_raw(callback_state);
            AudioComponentInstanceDispose(unit);
            anyhow::bail!(
                "Failed to set render callback: {}",
                osstatus_description(status)
            );
        }

        // 8. Set input callback (mic data receiver)
        let input_cb = AURenderCallbackStruct {
            input_proc: input_callback,
            input_proc_ref_con: callback_state as *mut c_void,
        };

        let status = AudioUnitSetProperty(
            unit,
            K_AUDIO_OUTPUT_UNIT_PROPERTY_SET_INPUT_CALLBACK,
            K_AUDIO_UNIT_SCOPE_GLOBAL,
            1, // bus 1 = input
            &input_cb as *const AURenderCallbackStruct as *const c_void,
            std::mem::size_of::<AURenderCallbackStruct>() as u32,
        );
        if status != 0 {
            let _ = Box::from_raw(callback_state);
            AudioComponentInstanceDispose(unit);
            anyhow::bail!(
                "Failed to set input callback: {}",
                osstatus_description(status)
            );
        }

        // 9. Initialize and start
        let status = AudioUnitInitialize(unit);
        if status != 0 {
            let _ = Box::from_raw(callback_state);
            AudioComponentInstanceDispose(unit);
            anyhow::bail!(
                "AudioUnitInitialize failed: {}",
                osstatus_description(status)
            );
        }

        let status = AudioOutputUnitStart(unit);
        if status != 0 {
            AudioUnitUninitialize(unit);
            let _ = Box::from_raw(callback_state);
            AudioComponentInstanceDispose(unit);
            anyhow::bail!(
                "AudioOutputUnitStart failed: {}",
                osstatus_description(status)
            );
        }

        tracing::info!(
            "VPIO AudioUnit started: {}Hz, {}ch, {:?}",
            config.sample_rate,
            config.channels,
            config.sample_format
        );

        Ok(AudioVoiceIO {
            unit,
            input_rx,
            output_tx,
            config,
            _callback_state: callback_state,
        })
    }

    /// Read the next audio frame from the mic (blocks until data is available).
    ///
    /// The returned frame has AEC applied — speaker audio is cancelled out.
    pub fn read(&self) -> Result<AudioFrame> {
        self.input_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("VPIO input stream closed"))
    }

    /// Try to read an audio frame without blocking.
    pub fn try_read(&self) -> Option<AudioFrame> {
        self.input_rx.try_recv().ok()
    }

    /// Write an audio frame to the speaker.
    ///
    /// This data also serves as the AEC reference signal.
    pub fn write(&self, frame: &AudioFrame) -> Result<()> {
        self.output_tx
            .send(frame.data.clone())
            .map_err(|_| anyhow::anyhow!("VPIO output stream closed"))
    }

    /// Write raw PCM bytes to the speaker.
    pub fn write_raw(&self, data: Vec<u8>) -> Result<()> {
        self.output_tx
            .send(data)
            .map_err(|_| anyhow::anyhow!("VPIO output stream closed"))
    }

    /// Get the audio config.
    pub fn config(&self) -> &AudioConfig {
        &self.config
    }
}

impl Drop for AudioVoiceIO {
    fn drop(&mut self) {
        unsafe {
            AudioOutputUnitStop(self.unit);
            AudioUnitUninitialize(self.unit);
            // Drop the callback state — safe because callbacks have stopped
            let _ = Box::from_raw(self._callback_state);
            AudioComponentInstanceDispose(self.unit);
        }
        tracing::debug!("VPIO AudioUnit disposed");
    }
}

// =============================================================================
// Helpers
// =============================================================================

fn config_to_asbd(config: &AudioConfig) -> AudioStreamBasicDescription {
    let bytes_per_sample = config.sample_format.bytes_per_sample() as u32;
    let bytes_per_frame = bytes_per_sample * config.channels as u32;

    let format_flags = match config.sample_format {
        SampleFormat::I16 => K_AUDIO_FORMAT_FLAG_IS_SIGNED_INTEGER | K_AUDIO_FORMAT_FLAG_IS_PACKED,
        SampleFormat::F32 => K_AUDIO_FORMAT_FLAG_IS_FLOAT | K_AUDIO_FORMAT_FLAG_IS_PACKED,
    };

    AudioStreamBasicDescription {
        sample_rate: config.sample_rate as f64,
        format_id: K_AUDIO_FORMAT_LINEAR_PCM,
        format_flags,
        bytes_per_packet: bytes_per_frame,
        frames_per_packet: 1,
        bytes_per_frame,
        channels_per_frame: config.channels as u32,
        bits_per_channel: bytes_per_sample * 8,
        reserved: 0,
    }
}

fn osstatus_description(status: i32) -> String {
    if status == 0 {
        return "noErr".to_string();
    }
    // Try to decode as FourCC
    let bytes = status.to_be_bytes();
    if bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        format!(
            "OSStatus {} ('{}')",
            status,
            String::from_utf8_lossy(&bytes)
        )
    } else {
        format!("OSStatus {}", status)
    }
}

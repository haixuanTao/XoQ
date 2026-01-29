//! NVIDIA NVENC encoder implementation.

use std::ffi::c_void;
use std::ptr;
use std::sync::Arc;

use cudarc::driver::CudaDevice;

use nvidia_sys::{
    GUID, NVENCAPI_VERSION,
    NV_ENC_BUFFER_FORMAT, NV_ENC_CONFIG, NV_ENC_CONFIG_VER,
    NV_ENC_CREATE_BITSTREAM_BUFFER, NV_ENC_CREATE_BITSTREAM_BUFFER_VER,
    NV_ENC_CREATE_INPUT_BUFFER, NV_ENC_CREATE_INPUT_BUFFER_VER,
    NV_ENC_DEVICE_TYPE, NV_ENC_INITIALIZE_PARAMS, NV_ENC_INITIALIZE_PARAMS_VER,
    NV_ENC_LOCK_BITSTREAM, NV_ENC_LOCK_BITSTREAM_VER,
    NV_ENC_LOCK_INPUT_BUFFER, NV_ENC_LOCK_INPUT_BUFFER_VER,
    NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS, NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER,
    NV_ENC_PIC_FLAGS, NV_ENC_PIC_PARAMS, NV_ENC_PIC_PARAMS_VER,
    NV_ENC_PIC_STRUCT, NV_ENC_PIC_TYPE,
    NV_ENC_PRESET_CONFIG, NV_ENC_PRESET_CONFIG_VER,
    NV_ENC_TUNING_INFO,
    NV_ENC_CODEC_H264_GUID, NV_ENC_CODEC_HEVC_GUID, NV_ENC_CODEC_AV1_GUID,
    NV_ENC_PRESET_P1_GUID, NV_ENC_PRESET_P2_GUID, NV_ENC_PRESET_P4_GUID,
    NV_ENC_PRESET_P5_GUID, NV_ENC_PRESET_P7_GUID,
};

use super::api::{ENCODE_API, NvencStatusExt};
use crate::{
    Codec, CodecError, EncodeParams, EncodedPacket, EncoderConfig, EncoderPreset,
    PixelFormat, TuningMode, VideoEncoder, VideoFrameData,
};

/// NVIDIA NVENC hardware encoder.
///
/// Uses NVIDIA's dedicated hardware encoder available on GeForce, Quadro,
/// and Tesla GPUs to perform hardware-accelerated video encoding.
pub struct NvencEncoder {
    /// Raw encoder pointer.
    ptr: *mut c_void,
    /// CUDA device.
    _cuda_device: Arc<CudaDevice>,
    /// Input buffer pointer.
    input_buffer: *mut c_void,
    /// Output bitstream buffer pointer.
    output_bitstream: *mut c_void,
    /// Buffer format being used.
    buffer_format: NV_ENC_BUFFER_FORMAT,
    /// Codec being used.
    codec: Codec,
    /// Frame width.
    width: u32,
    /// Frame height.
    height: u32,
    /// Current frame index.
    frame_index: u64,
    /// Input buffer pitch (stride).
    input_pitch: u32,
}

// SAFETY: The NVENC encoder can be sent between threads.
// The underlying CUDA context handles thread synchronization.
unsafe impl Send for NvencEncoder {}

// SAFETY: The encoder requires &mut self for all operations,
// so concurrent access is prevented by Rust's borrowing rules.
unsafe impl Sync for NvencEncoder {}

impl Drop for NvencEncoder {
    fn drop(&mut self) {
        // Send EOS to flush the encoder
        let _ = self.send_eos();

        // Destroy input buffer
        if !self.input_buffer.is_null() {
            unsafe { (ENCODE_API.destroy_input_buffer)(self.ptr, self.input_buffer) }
                .result(self.ptr)
                .ok();
        }

        // Destroy output bitstream buffer
        if !self.output_bitstream.is_null() {
            unsafe { (ENCODE_API.destroy_bitstream_buffer)(self.ptr, self.output_bitstream) }
                .result(self.ptr)
                .ok();
        }

        // Destroy encoder
        if !self.ptr.is_null() {
            unsafe { (ENCODE_API.destroy_encoder)(self.ptr) }
                .result_without_string()
                .ok();
        }
    }
}

impl NvencEncoder {
    /// Create a new NVENC encoder with the given configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No CUDA-capable device is found
    /// - The device doesn't support encoding
    /// - The requested codec is not supported
    /// - Initialization fails
    pub fn new(config: EncoderConfig) -> Result<Self, CodecError> {
        Self::with_cuda_device(config, 0)
    }

    /// Create a new NVENC encoder on a specific CUDA device.
    pub fn with_cuda_device(config: EncoderConfig, device_ordinal: usize) -> Result<Self, CodecError> {
        // Initialize CUDA device
        let cuda_device = CudaDevice::new(device_ordinal)
            .map_err(|e| CodecError::CudaError(e.to_string()))?;

        Self::with_cuda(config, cuda_device)
    }

    /// Create a new NVENC encoder with an existing CUDA device.
    pub fn with_cuda(config: EncoderConfig, cuda_device: Arc<CudaDevice>) -> Result<Self, CodecError> {
        // Open encode session
        let mut encoder_ptr = ptr::null_mut();
        let mut session_params = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS {
            version: NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER,
            deviceType: NV_ENC_DEVICE_TYPE::NV_ENC_DEVICE_TYPE_CUDA,
            apiVersion: NVENCAPI_VERSION,
            device: (*cuda_device.cu_primary_ctx()) as *mut c_void,
            ..Default::default()
        };

        let result = unsafe { (ENCODE_API.open_encode_session_ex)(&mut session_params, &mut encoder_ptr) };
        if let Err(e) = result.result_without_string() {
            // Destroy encoder on error
            if !encoder_ptr.is_null() {
                unsafe { (ENCODE_API.destroy_encoder)(encoder_ptr) }.result_without_string().ok();
            }
            return Err(e);
        }

        // Get codec GUID
        let encode_guid = codec_to_guid(config.codec)?;

        // Get preset GUID
        let preset_guid = preset_to_guid(config.preset);

        // Get tuning info
        let tuning_info = tuning_to_nvenc(config.tuning);

        // Get preset config
        let mut preset_config = NV_ENC_PRESET_CONFIG {
            version: NV_ENC_PRESET_CONFIG_VER,
            presetCfg: NV_ENC_CONFIG {
                version: NV_ENC_CONFIG_VER,
                ..Default::default()
            },
            ..Default::default()
        };

        unsafe {
            (ENCODE_API.get_encode_preset_config_ex)(
                encoder_ptr,
                encode_guid,
                preset_guid,
                tuning_info,
                &mut preset_config,
            )
        }
        .result(encoder_ptr)?;

        // Configure encoding parameters
        let mut encode_config = preset_config.presetCfg;

        // Set GOP length
        if config.gop_length > 0 {
            encode_config.gopLength = config.gop_length;
        }

        // Set B-frames
        encode_config.frameIntervalP = (config.b_frames + 1) as i32;

        // Set bitrate
        encode_config.rcParams.averageBitRate = config.bitrate;
        encode_config.rcParams.maxBitRate = config.max_bitrate;

        // Determine buffer format (use ARGB for simplicity, convert input if needed)
        let buffer_format = NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_ARGB;

        // Initialize encoder
        let mut init_params = NV_ENC_INITIALIZE_PARAMS {
            version: NV_ENC_INITIALIZE_PARAMS_VER,
            encodeGUID: encode_guid,
            presetGUID: preset_guid,
            encodeWidth: config.width,
            encodeHeight: config.height,
            darWidth: config.width,
            darHeight: config.height,
            frameRateNum: config.framerate.0,
            frameRateDen: config.framerate.1,
            enablePTD: 1, // Enable picture type decision
            tuningInfo: tuning_info,
            encodeConfig: &mut encode_config,
            ..Default::default()
        };

        unsafe { (ENCODE_API.initialize_encoder)(encoder_ptr, &mut init_params) }
            .result(encoder_ptr)?;

        // Create input buffer
        let mut create_input_buffer = NV_ENC_CREATE_INPUT_BUFFER {
            version: NV_ENC_CREATE_INPUT_BUFFER_VER,
            width: config.width,
            height: config.height,
            bufferFmt: buffer_format,
            ..Default::default()
        };

        unsafe { (ENCODE_API.create_input_buffer)(encoder_ptr, &mut create_input_buffer) }
            .result(encoder_ptr)?;

        let input_buffer = create_input_buffer.inputBuffer;

        // Create output bitstream buffer
        let mut create_bitstream_buffer = NV_ENC_CREATE_BITSTREAM_BUFFER {
            version: NV_ENC_CREATE_BITSTREAM_BUFFER_VER,
            ..Default::default()
        };

        unsafe { (ENCODE_API.create_bitstream_buffer)(encoder_ptr, &mut create_bitstream_buffer) }
            .result(encoder_ptr)?;

        let output_bitstream = create_bitstream_buffer.bitstreamBuffer;

        Ok(Self {
            ptr: encoder_ptr,
            _cuda_device: cuda_device,
            input_buffer,
            output_bitstream,
            buffer_format,
            codec: config.codec,
            width: config.width,
            height: config.height,
            frame_index: 0,
            input_pitch: config.width,
        })
    }

    /// Send end-of-stream signal to flush the encoder.
    fn send_eos(&mut self) -> Result<(), CodecError> {
        let mut pic_params = NV_ENC_PIC_PARAMS {
            version: NV_ENC_PIC_PARAMS_VER,
            encodePicFlags: NV_ENC_PIC_FLAGS::NV_ENC_PIC_FLAG_EOS as u32,
            ..Default::default()
        };

        unsafe { (ENCODE_API.encode_picture)(self.ptr, &mut pic_params) }
            .result(self.ptr)
    }

    /// Lock the input buffer and copy frame data.
    fn copy_frame_to_input(&mut self, frame: &dyn VideoFrameData) -> Result<(), CodecError> {
        // Convert frame to ARGB format
        let argb_data = convert_to_argb(frame)?;

        // Lock input buffer
        let mut lock_params = NV_ENC_LOCK_INPUT_BUFFER {
            version: NV_ENC_LOCK_INPUT_BUFFER_VER,
            inputBuffer: self.input_buffer,
            ..Default::default()
        };

        unsafe { (ENCODE_API.lock_input_buffer)(self.ptr, &mut lock_params) }
            .result(self.ptr)?;

        // Copy data to buffer
        let pitch = lock_params.pitch;
        self.input_pitch = pitch;

        let data_ptr = lock_params.bufferDataPtr as *mut u8;
        let src_stride = frame.width() as usize * 4; // ARGB = 4 bytes per pixel

        // Copy row by row to handle pitch
        for y in 0..frame.height() as usize {
            let src_offset = y * src_stride;
            let dst_offset = y * pitch as usize;
            unsafe {
                ptr::copy_nonoverlapping(
                    argb_data.as_ptr().add(src_offset),
                    data_ptr.add(dst_offset),
                    src_stride,
                );
            }
        }

        // Unlock input buffer
        unsafe { (ENCODE_API.unlock_input_buffer)(self.ptr, self.input_buffer) }
            .result(self.ptr)?;

        Ok(())
    }

    /// Encode a frame and return the encoded data.
    fn encode_frame_internal(&mut self, timestamp_us: u64, force_keyframe: bool) -> Result<EncodedPacket, CodecError> {
        // Set up picture parameters
        let mut pic_params = NV_ENC_PIC_PARAMS {
            version: NV_ENC_PIC_PARAMS_VER,
            inputWidth: self.width,
            inputHeight: self.height,
            inputPitch: self.input_pitch,
            inputBuffer: self.input_buffer,
            outputBitstream: self.output_bitstream,
            bufferFmt: self.buffer_format,
            pictureStruct: NV_ENC_PIC_STRUCT::NV_ENC_PIC_STRUCT_FRAME,
            inputTimeStamp: timestamp_us,
            encodePicFlags: if force_keyframe {
                NV_ENC_PIC_FLAGS::NV_ENC_PIC_FLAG_FORCEIDR as u32
            } else {
                0
            },
            ..Default::default()
        };

        // Encode the picture
        unsafe { (ENCODE_API.encode_picture)(self.ptr, &mut pic_params) }
            .result(self.ptr)?;

        // Lock and read the output bitstream
        let mut lock_bitstream = NV_ENC_LOCK_BITSTREAM {
            version: NV_ENC_LOCK_BITSTREAM_VER,
            outputBitstream: self.output_bitstream,
            ..Default::default()
        };

        unsafe { (ENCODE_API.lock_bitstream)(self.ptr, &mut lock_bitstream) }
            .result(self.ptr)?;

        // Copy encoded data
        let data_size = lock_bitstream.bitstreamSizeInBytes as usize;
        let data_ptr = lock_bitstream.bitstreamBufferPtr as *const u8;
        let data = unsafe { std::slice::from_raw_parts(data_ptr, data_size) }.to_vec();

        let is_keyframe = matches!(
            lock_bitstream.pictureType,
            NV_ENC_PIC_TYPE::NV_ENC_PIC_TYPE_IDR | NV_ENC_PIC_TYPE::NV_ENC_PIC_TYPE_I
        );

        let frame_idx = self.frame_index;
        self.frame_index += 1;

        // Unlock bitstream
        unsafe { (ENCODE_API.unlock_bitstream)(self.ptr, self.output_bitstream) }
            .result(self.ptr)?;

        Ok(EncodedPacket::new(data, timestamp_us, is_keyframe, frame_idx))
    }
}

impl VideoEncoder for NvencEncoder {
    fn encode(&mut self, frame: &dyn VideoFrameData) -> Result<EncodedPacket, CodecError> {
        self.encode_with_params(frame, EncodeParams::default())
    }

    fn encode_with_params(
        &mut self,
        frame: &dyn VideoFrameData,
        params: EncodeParams,
    ) -> Result<EncodedPacket, CodecError> {
        // Validate frame dimensions
        if frame.width() != self.width || frame.height() != self.height {
            return Err(CodecError::InvalidDimensions {
                width: frame.width(),
                height: frame.height(),
            });
        }

        // Copy frame data to input buffer
        self.copy_frame_to_input(frame)?;

        // Encode
        let timestamp = params.timestamp_us.unwrap_or_else(|| frame.timestamp_us());
        self.encode_frame_internal(timestamp, params.force_keyframe)
    }

    fn flush(&mut self) -> Result<Vec<EncodedPacket>, CodecError> {
        // Send EOS to flush
        self.send_eos()?;

        // For now, we don't buffer frames (no B-frames by default),
        // so there's nothing to flush
        Ok(Vec::new())
    }

    fn codec(&self) -> Codec {
        self.codec
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

// ============================================================================
// Helper functions
// ============================================================================

fn codec_to_guid(codec: Codec) -> Result<GUID, CodecError> {
    match codec {
        Codec::H264 => Ok(NV_ENC_CODEC_H264_GUID),
        Codec::Hevc => Ok(NV_ENC_CODEC_HEVC_GUID),
        Codec::Av1 => Ok(NV_ENC_CODEC_AV1_GUID),
    }
}

fn preset_to_guid(preset: EncoderPreset) -> GUID {
    match preset {
        EncoderPreset::Fastest => NV_ENC_PRESET_P1_GUID,
        EncoderPreset::Fast => NV_ENC_PRESET_P2_GUID,
        EncoderPreset::Medium => NV_ENC_PRESET_P4_GUID,
        EncoderPreset::Slow => NV_ENC_PRESET_P5_GUID,
        EncoderPreset::Slowest => NV_ENC_PRESET_P7_GUID,
    }
}

fn tuning_to_nvenc(tuning: TuningMode) -> NV_ENC_TUNING_INFO {
    match tuning {
        TuningMode::HighQuality => NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_HIGH_QUALITY,
        TuningMode::LowLatency => NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_LOW_LATENCY,
        TuningMode::UltraLowLatency => NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
        TuningMode::Lossless => NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_LOSSLESS,
    }
}

/// Convert frame data to ARGB format for NVENC input.
fn convert_to_argb(frame: &dyn VideoFrameData) -> Result<Vec<u8>, CodecError> {
    let w = frame.width() as usize;
    let h = frame.height() as usize;
    let data = frame.data();

    match frame.pixel_format() {
        PixelFormat::Argb => Ok(data.to_vec()),
        PixelFormat::Rgba => {
            // RGBA -> ARGB: swap A from end to beginning
            let mut argb = vec![0u8; w * h * 4];
            for i in 0..(w * h) {
                argb[i * 4] = data[i * 4 + 3]; // A
                argb[i * 4 + 1] = data[i * 4]; // R
                argb[i * 4 + 2] = data[i * 4 + 1]; // G
                argb[i * 4 + 3] = data[i * 4 + 2]; // B
            }
            Ok(argb)
        }
        PixelFormat::Rgb => {
            // RGB -> ARGB: add alpha channel
            let mut argb = vec![0u8; w * h * 4];
            for i in 0..(w * h) {
                argb[i * 4] = 255; // A
                argb[i * 4 + 1] = data[i * 3]; // R
                argb[i * 4 + 2] = data[i * 3 + 1]; // G
                argb[i * 4 + 3] = data[i * 3 + 2]; // B
            }
            Ok(argb)
        }
        PixelFormat::Bgr => {
            // BGR -> ARGB: swap B/R and add alpha
            let mut argb = vec![0u8; w * h * 4];
            for i in 0..(w * h) {
                argb[i * 4] = 255; // A
                argb[i * 4 + 1] = data[i * 3 + 2]; // R
                argb[i * 4 + 2] = data[i * 3 + 1]; // G
                argb[i * 4 + 3] = data[i * 3]; // B
            }
            Ok(argb)
        }
        PixelFormat::Bgra => {
            // BGRA -> ARGB: swap B/R and move A
            let mut argb = vec![0u8; w * h * 4];
            for i in 0..(w * h) {
                argb[i * 4] = data[i * 4 + 3]; // A
                argb[i * 4 + 1] = data[i * 4 + 2]; // R
                argb[i * 4 + 2] = data[i * 4 + 1]; // G
                argb[i * 4 + 3] = data[i * 4]; // B
            }
            Ok(argb)
        }
        PixelFormat::Abgr => {
            // ABGR -> ARGB: swap B/R
            let mut argb = vec![0u8; w * h * 4];
            for i in 0..(w * h) {
                argb[i * 4] = data[i * 4]; // A
                argb[i * 4 + 1] = data[i * 4 + 3]; // R
                argb[i * 4 + 2] = data[i * 4 + 2]; // G
                argb[i * 4 + 3] = data[i * 4 + 1]; // B
            }
            Ok(argb)
        }
        PixelFormat::Nv12 | PixelFormat::I420 => {
            Err(CodecError::ConversionError(
                "NV12/I420 to ARGB conversion not implemented; use ARGB input format".into()
            ))
        }
    }
}

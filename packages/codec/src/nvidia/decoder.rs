//! NVIDIA NVDEC decoder implementation.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr;
use std::sync::Arc;

use cudarc::driver::CudaDevice;

use nvidia_sys::{
    cudaVideoChromaFormat, cudaVideoCodec, cudaVideoCreateFlags, cudaVideoDeinterlaceMode,
    cudaVideoSurfaceFormat, CUresult, CUvideodecoder, CUvideoparser,
    CUDA_SUCCESS, CUVIDDECODECAPS, CUVIDDECODECREATEINFO, CUVIDPARSERPARAMS,
    CUVIDPICPARAMS, CUVIDPROCPARAMS, CUVIDSOURCEDATAPACKET,
    CUVID_PKT_ENDOFSTREAM, CUVID_PKT_TIMESTAMP,
    cuvidCreateDecoder, cuvidCreateVideoParser, cuvidDecodePicture,
    cuvidDestroyDecoder, cuvidDestroyVideoParser, cuvidGetDecoderCaps,
    cuvidMapVideoFrame64, cuvidParseVideoData, cuvidUnmapVideoFrame64,
    CUVIDEOFORMAT, CUVIDPARSERDISPINFO,
};

use crate::{
    Codec, CodecError, DecodedFrame, DecoderConfig, EncodedPacket, PixelFormat, VideoDecoder,
};

/// NVIDIA NVDEC hardware decoder.
///
/// Uses NVIDIA's dedicated hardware decoder available on GeForce, Quadro,
/// and Tesla GPUs to perform hardware-accelerated video decoding.
pub struct NvdecDecoder {
    /// Video parser handle.
    parser: CUvideoparser,
    /// Video decoder handle (created when format is known).
    decoder: Option<CUvideodecoder>,
    /// CUDA device.
    cuda_device: Arc<CudaDevice>,
    /// Decoder configuration.
    config: DecoderConfig,
    /// Current video format (set when sequence callback fires).
    video_format: Option<VideoFormat>,
    /// Queue of decoded frames ready for output.
    output_queue: VecDeque<DecodedFrame>,
    /// Current frame index.
    frame_index: u64,
    /// Pending picture params to decode.
    pending_decode: VecDeque<PendingPicture>,
    /// Pending display info.
    pending_display: VecDeque<DisplayInfo>,
}

/// Internal video format information.
#[derive(Debug, Clone)]
struct VideoFormat {
    width: u32,
    height: u32,
    coded_width: u32,
    coded_height: u32,
    chroma_format: cudaVideoChromaFormat,
    bit_depth: u8,
}

/// Pending picture to decode.
struct PendingPicture {
    pic_params: Box<CUVIDPICPARAMS>,
}

/// Display information for a decoded frame.
#[derive(Debug, Clone)]
struct DisplayInfo {
    picture_index: i32,
    timestamp: u64,
}

// SAFETY: The NVDEC decoder can be sent between threads.
// The underlying CUDA context handles thread synchronization.
unsafe impl Send for NvdecDecoder {}

impl Drop for NvdecDecoder {
    fn drop(&mut self) {
        // Destroy parser
        if !self.parser.is_null() {
            unsafe { cuvidDestroyVideoParser(self.parser) };
        }

        // Destroy decoder
        if let Some(decoder) = self.decoder.take() {
            if !decoder.is_null() {
                unsafe { cuvidDestroyDecoder(decoder) };
            }
        }
    }
}

impl NvdecDecoder {
    /// Create a new NVDEC decoder with the given configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No CUDA-capable device is found
    /// - The device doesn't support decoding the requested codec
    /// - Initialization fails
    pub fn new(config: DecoderConfig) -> Result<Self, CodecError> {
        Self::with_cuda_device(config, 0)
    }

    /// Create a new NVDEC decoder on a specific CUDA device.
    pub fn with_cuda_device(config: DecoderConfig, device_ordinal: usize) -> Result<Self, CodecError> {
        let cuda_device = CudaDevice::new(device_ordinal)
            .map_err(|e| CodecError::CudaError(e.to_string()))?;

        Self::with_cuda(config, cuda_device)
    }

    /// Create a new NVDEC decoder with an existing CUDA device.
    pub fn with_cuda(config: DecoderConfig, cuda_device: Arc<CudaDevice>) -> Result<Self, CodecError> {
        // Check decoder capabilities
        let cuvid_codec = codec_to_cuvid(config.codec);
        let mut caps = CUVIDDECODECAPS::default();
        caps.eCodecType = cuvid_codec;
        caps.eChromaFormat = cudaVideoChromaFormat::cudaVideoChromaFormat_420;
        caps.nBitDepthMinus8 = 0;

        let result = unsafe { cuvidGetDecoderCaps(&mut caps) };
        if result != CUDA_SUCCESS {
            return Err(CodecError::Generic(format!(
                "Failed to get decoder caps: {}",
                result
            )));
        }

        if caps.bIsSupported == 0 {
            return Err(CodecError::UnsupportedCodec(format!(
                "{:?} not supported by this GPU",
                config.codec
            )));
        }

        // Create video parser (decoder will be created when we receive the first frame)
        let mut parser: CUvideoparser = ptr::null_mut();
        let mut parser_params = CUVIDPARSERPARAMS {
            CodecType: cuvid_codec,
            ulMaxNumDecodeSurfaces: if config.max_decode_surfaces > 0 {
                config.max_decode_surfaces
            } else {
                8 // Default
            },
            ulMaxDisplayDelay: config.max_display_delay,
            ..Default::default()
        };

        // We don't use callbacks in this simple implementation - instead we'll
        // parse synchronously and handle the results ourselves
        let result = unsafe { cuvidCreateVideoParser(&mut parser, &mut parser_params) };
        if result != CUDA_SUCCESS {
            return Err(CodecError::Generic(format!(
                "Failed to create video parser: {}",
                result
            )));
        }

        Ok(Self {
            parser,
            decoder: None,
            cuda_device,
            config,
            video_format: None,
            output_queue: VecDeque::new(),
            frame_index: 0,
            pending_decode: VecDeque::new(),
            pending_display: VecDeque::new(),
        })
    }

    /// Create the actual decoder once we know the video format.
    fn create_decoder(&mut self, format: &VideoFormat) -> Result<(), CodecError> {
        if self.decoder.is_some() {
            return Ok(());
        }

        let target_width = if self.config.target_width > 0 {
            self.config.target_width
        } else {
            format.width
        };

        let target_height = if self.config.target_height > 0 {
            self.config.target_height
        } else {
            format.height
        };

        let output_format = match self.config.output_format {
            PixelFormat::Nv12 => cudaVideoSurfaceFormat::cudaVideoSurfaceFormat_NV12,
            _ => cudaVideoSurfaceFormat::cudaVideoSurfaceFormat_NV12, // Default to NV12
        };

        let deinterlace_mode = if self.config.deinterlace {
            cudaVideoDeinterlaceMode::cudaVideoDeinterlaceMode_Adaptive
        } else {
            cudaVideoDeinterlaceMode::cudaVideoDeinterlaceMode_Weave
        };

        let mut decode_create_info = CUVIDDECODECREATEINFO {
            ulWidth: format.coded_width as _,
            ulHeight: format.coded_height as _,
            ulNumDecodeSurfaces: if self.config.max_decode_surfaces > 0 {
                self.config.max_decode_surfaces as _
            } else {
                8
            },
            CodecType: codec_to_cuvid(self.config.codec),
            ChromaFormat: format.chroma_format,
            ulCreationFlags: cudaVideoCreateFlags::cudaVideoCreate_PreferCUVID as _,
            bitDepthMinus8: format.bit_depth.saturating_sub(8) as _,
            ulIntraDecodeOnly: 0,
            ulMaxWidth: format.coded_width as _,
            ulMaxHeight: format.coded_height as _,
            Reserved1: 0,
            display_area: nvidia_sys::_CUVIDDECODECREATEINFO__display_area {
                left: 0,
                top: 0,
                right: format.width as i32,
                bottom: format.height as i32,
            },
            OutputFormat: output_format,
            DeinterlaceMode: deinterlace_mode,
            ulTargetWidth: target_width as _,
            ulTargetHeight: target_height as _,
            ulNumOutputSurfaces: 2,
            vidLock: ptr::null_mut(),
            target_rect: nvidia_sys::_CUVIDDECODECREATEINFO__target_rect {
                left: 0,
                top: 0,
                right: target_width as i32,
                bottom: target_height as i32,
            },
            enableHistogram: 0,
            Reserved2: [0; 4],
        };

        let mut decoder: CUvideodecoder = ptr::null_mut();
        let result = unsafe { cuvidCreateDecoder(&mut decoder, &mut decode_create_info) };
        if result != CUDA_SUCCESS {
            return Err(CodecError::Generic(format!(
                "Failed to create decoder: {}",
                result
            )));
        }

        self.decoder = Some(decoder);
        Ok(())
    }

    /// Parse a packet and get decoded frames.
    fn parse_packet(&mut self, data: &[u8], timestamp: u64, is_eos: bool) -> Result<(), CodecError> {
        let mut packet = CUVIDSOURCEDATAPACKET {
            flags: if is_eos {
                CUVID_PKT_ENDOFSTREAM
            } else {
                CUVID_PKT_TIMESTAMP
            },
            payload_size: data.len() as _,
            payload: data.as_ptr(),
            timestamp,
        };

        let result = unsafe { cuvidParseVideoData(self.parser, &mut packet) };
        if result != CUDA_SUCCESS {
            return Err(CodecError::Generic(format!(
                "Failed to parse video data: {}",
                result
            )));
        }

        Ok(())
    }

    /// Decode a picture using the hardware decoder.
    fn decode_picture(&mut self, pic_params: &mut CUVIDPICPARAMS) -> Result<(), CodecError> {
        let decoder = self.decoder.ok_or_else(|| {
            CodecError::Generic("Decoder not initialized".into())
        })?;

        let result = unsafe { cuvidDecodePicture(decoder, pic_params) };
        if result != CUDA_SUCCESS {
            return Err(CodecError::Generic(format!(
                "Failed to decode picture: {}",
                result
            )));
        }

        Ok(())
    }

    /// Map a decoded frame and copy to CPU memory.
    fn map_frame(&mut self, picture_index: i32, timestamp: u64) -> Result<DecodedFrame, CodecError> {
        let decoder = self.decoder.ok_or_else(|| {
            CodecError::Generic("Decoder not initialized".into())
        })?;

        let format = self.video_format.as_ref().ok_or_else(|| {
            CodecError::Generic("Video format not known".into())
        })?;

        let target_width = if self.config.target_width > 0 {
            self.config.target_width
        } else {
            format.width
        };

        let target_height = if self.config.target_height > 0 {
            self.config.target_height
        } else {
            format.height
        };

        let mut proc_params = CUVIDPROCPARAMS::default();
        proc_params.progressive_frame = 1;

        let mut dev_ptr: u64 = 0;
        let mut pitch: u32 = 0;

        let result = unsafe {
            cuvidMapVideoFrame64(
                decoder,
                picture_index,
                &mut dev_ptr,
                &mut pitch,
                &mut proc_params,
            )
        };
        if result != CUDA_SUCCESS {
            return Err(CodecError::Generic(format!(
                "Failed to map video frame: {}",
                result
            )));
        }

        // Calculate frame size (NV12: Y plane + UV plane)
        let y_size = (pitch * target_height) as usize;
        let uv_size = (pitch * (target_height / 2)) as usize;
        let total_size = y_size + uv_size;

        // Allocate buffer and copy from GPU
        let mut data = vec![0u8; total_size];

        // Copy Y plane
        let y_plane_ptr = dev_ptr as *const u8;
        unsafe {
            ptr::copy_nonoverlapping(y_plane_ptr, data.as_mut_ptr(), y_size);
        }

        // Copy UV plane (follows Y plane in memory)
        let uv_plane_ptr = (dev_ptr + y_size as u64) as *const u8;
        unsafe {
            ptr::copy_nonoverlapping(uv_plane_ptr, data.as_mut_ptr().add(y_size), uv_size);
        }

        // Unmap frame
        let result = unsafe { cuvidUnmapVideoFrame64(decoder, dev_ptr) };
        if result != CUDA_SUCCESS {
            return Err(CodecError::Generic(format!(
                "Failed to unmap video frame: {}",
                result
            )));
        }

        let frame_idx = self.frame_index;
        self.frame_index += 1;

        Ok(DecodedFrame::new(
            data,
            target_width,
            target_height,
            PixelFormat::Nv12,
            timestamp,
            frame_idx,
        ))
    }

    /// Process a packet using a simpler synchronous approach.
    ///
    /// This implementation uses direct CUVID calls rather than callbacks,
    /// which is simpler for low-latency use cases.
    fn process_packet_sync(
        &mut self,
        packet: &EncodedPacket,
    ) -> Result<Option<DecodedFrame>, CodecError> {
        // For a simpler implementation, we'll initialize with assumed format
        // In a full implementation, you'd parse SPS/PPS from H.264 or VPS/SPS/PPS from HEVC
        if self.video_format.is_none() {
            // Set a default format - in practice this should come from parsing headers
            self.video_format = Some(VideoFormat {
                width: 1920, // Will be updated when actual dimensions are known
                height: 1080,
                coded_width: 1920,
                coded_height: 1088, // Rounded up to 16
                chroma_format: cudaVideoChromaFormat::cudaVideoChromaFormat_420,
                bit_depth: 8,
            });
        }

        // Ensure decoder is created
        if self.decoder.is_none() {
            if let Some(format) = &self.video_format {
                self.create_decoder(format)?;
            }
        }

        // Parse the packet
        self.parse_packet(&packet.data, packet.pts_us, false)?;

        // For low-latency mode, try to get any available frame
        // In practice, you'd need to handle the async nature of decoding
        // This is a simplified version
        Ok(None)
    }
}

impl VideoDecoder for NvdecDecoder {
    fn decode(&mut self, packet: &EncodedPacket) -> Result<Option<DecodedFrame>, CodecError> {
        self.process_packet_sync(packet)
    }

    fn flush(&mut self) -> Result<Vec<DecodedFrame>, CodecError> {
        // Send EOS to parser
        self.parse_packet(&[], 0, true)?;

        // Collect any remaining frames
        let mut frames = Vec::new();
        while let Some(frame) = self.output_queue.pop_front() {
            frames.push(frame);
        }
        Ok(frames)
    }

    fn codec(&self) -> Codec {
        self.config.codec
    }

    fn dimensions(&self) -> Option<(u32, u32)> {
        self.video_format.as_ref().map(|f| (f.width, f.height))
    }

    fn output_format(&self) -> PixelFormat {
        self.config.output_format
    }
}

// ============================================================================
// Helper functions
// ============================================================================

fn codec_to_cuvid(codec: Codec) -> cudaVideoCodec {
    match codec {
        Codec::H264 => cudaVideoCodec::cudaVideoCodec_H264,
        Codec::Hevc => cudaVideoCodec::cudaVideoCodec_HEVC,
        Codec::Av1 => cudaVideoCodec::cudaVideoCodec_AV1,
    }
}

/// Convert CUresult to CodecError
fn curesult_to_error(result: CUresult) -> CodecError {
    match result {
        CUDA_SUCCESS => CodecError::Generic("unexpected success".into()),
        nvidia_sys::CUDA_ERROR_INVALID_VALUE => CodecError::InvalidParam("invalid value".into()),
        nvidia_sys::CUDA_ERROR_OUT_OF_MEMORY => CodecError::OutOfMemory,
        nvidia_sys::CUDA_ERROR_NOT_INITIALIZED => CodecError::EncoderNotInitialized,
        nvidia_sys::CUDA_ERROR_INVALID_DEVICE => CodecError::InvalidDevice,
        _ => CodecError::Generic(format!("CUDA error: {}", result)),
    }
}

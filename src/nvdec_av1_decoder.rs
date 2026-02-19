//! NVDEC hardware AV1 decoder.
//!
//! Decodes AV1 bitstream using NVIDIA's hardware decoder (NVDEC).
//! Supports:
//! - 8-bit: NV12 output → RGB conversion (for color frames)
//! - 10-bit: P016 output → raw Y-plane extraction (for depth frames)

use anyhow::Result;
use cudarc::driver::sys::CUresult;
use cudarc::driver::CudaContext;
use nvidia_video_codec_sdk::sys::cuviddec::*;
use nvidia_video_codec_sdk::sys::nvcuvid::*;
use std::ffi::c_void;
use std::ptr;

const CUDA_SUCCESS: CUresult = CUresult::CUDA_SUCCESS;

/// A decoded video frame from NVDEC.
pub struct DecodedFrame {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Raw pixel data (RGB u8 for 8-bit, or raw Y-plane u16 LE for 10-bit).
    pub data: Vec<u8>,
    /// Bits per component (8 or 10).
    pub bits_per_component: u8,
}

/// Internal decoded frame from NVDEC callback.
struct NvdecFrame {
    data: Vec<u8>,
    coded_width: u32,
    coded_height: u32,
    bit_depth: u8,
}

/// NVDEC hardware AV1 decoder.
pub struct NvdecAv1Decoder {
    parser: CUvideoparser,
    decoder: CUvideodecoder,
    _ctx: std::sync::Arc<CudaContext>,
    width: u32,
    height: u32,
    high_bitdepth: bool,
    bit_depth: u8,
    decoded_frames: Vec<NvdecFrame>,
}

impl NvdecAv1Decoder {
    /// Create a new NVDEC AV1 decoder.
    ///
    /// `high_bitdepth` controls the output surface format:
    /// - false: NV12 (8-bit) — for color frames
    /// - true: P016 (16-bit) — for 10-bit depth frames
    pub fn new(high_bitdepth: bool) -> Result<Self> {
        let ctx = CudaContext::new(0)
            .map_err(|e| anyhow::anyhow!("Failed to create CUDA context: {}", e))?;

        Ok(NvdecAv1Decoder {
            parser: ptr::null_mut(),
            decoder: ptr::null_mut(),
            _ctx: ctx,
            width: 0,
            height: 0,
            high_bitdepth,
            bit_depth: if high_bitdepth { 10 } else { 8 },
            decoded_frames: Vec::new(),
        })
    }

    /// Lazily create the CUVID parser.
    ///
    /// Must be called from `decode()` (not `new()`) so that `self` has a
    /// stable address — the parser stores a raw pointer to `self` for callbacks.
    fn ensure_parser(&mut self) -> Result<()> {
        if !self.parser.is_null() {
            return Ok(());
        }

        let mut parser_params: CUVIDPARSERPARAMS = unsafe { std::mem::zeroed() };
        parser_params.CodecType = cudaVideoCodec::cudaVideoCodec_AV1;
        parser_params.ulMaxNumDecodeSurfaces = 8;
        parser_params.ulMaxDisplayDelay = 0;
        parser_params.pUserData = self as *mut _ as *mut c_void;
        parser_params.pfnSequenceCallback = Some(Self::sequence_callback);
        parser_params.pfnDecodePicture = Some(Self::decode_callback);
        parser_params.pfnDisplayPicture = Some(Self::display_callback);

        let result = unsafe { cuvidCreateVideoParser(&mut self.parser, &mut parser_params) };
        if result != CUDA_SUCCESS {
            anyhow::bail!("Failed to create AV1 video parser: {:?}", result);
        }

        Ok(())
    }

    /// Decode AV1 OBU data and return the decoded frame.
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<DecodedFrame>> {
        self.ensure_parser()?;

        let mut packet: CUVIDSOURCEDATAPACKET = unsafe { std::mem::zeroed() };
        packet.payload = data.as_ptr();
        packet.payload_size = data.len() as u64;

        let result = unsafe { cuvidParseVideoData(self.parser, &mut packet) };
        if result != CUDA_SUCCESS {
            anyhow::bail!("Failed to parse AV1 data: {:?}", result);
        }

        if let Some(frame) = self.decoded_frames.pop() {
            let width = frame.coded_width;
            let height = frame.coded_height;

            let out = if frame.bit_depth > 8 {
                Self::extract_p016_y_plane(&frame.data, width as usize, height as usize)
            } else {
                Self::nv12_to_rgb(&frame.data, width as usize, height as usize)
            };

            Ok(Some(DecodedFrame {
                width,
                height,
                data: out,
                bits_per_component: frame.bit_depth,
            }))
        } else {
            Ok(None)
        }
    }

    /// Extract Y-plane from P016 surface.
    fn extract_p016_y_plane(data: &[u8], width: usize, height: usize) -> Vec<u8> {
        let stride = width * 2;
        let mut out = Vec::with_capacity(width * height * 2);
        for row in 0..height {
            let start = row * stride;
            let end = start + width * 2;
            if end <= data.len() {
                out.extend_from_slice(&data[start..end]);
            }
        }
        out
    }

    /// Convert NV12 surface to RGB.
    fn nv12_to_rgb(nv12: &[u8], width: usize, height: usize) -> Vec<u8> {
        let y_plane_size = width * height;
        let mut rgb = vec![0u8; width * height * 3];

        for y in 0..height {
            for x in 0..width {
                let y_val = nv12.get(y * width + x).copied().unwrap_or(0) as f32;
                let uv_idx = y_plane_size + (y / 2) * width + (x / 2) * 2;
                let u = nv12.get(uv_idx).copied().unwrap_or(128) as f32;
                let v = nv12.get(uv_idx + 1).copied().unwrap_or(128) as f32;

                let c = y_val - 16.0;
                let d = u - 128.0;
                let e = v - 128.0;

                let r = (1.164 * c + 1.596 * e).clamp(0.0, 255.0) as u8;
                let g = (1.164 * c - 0.392 * d - 0.813 * e).clamp(0.0, 255.0) as u8;
                let b = (1.164 * c + 2.017 * d).clamp(0.0, 255.0) as u8;

                let rgb_idx = (y * width + x) * 3;
                rgb[rgb_idx] = r;
                rgb[rgb_idx + 1] = g;
                rgb[rgb_idx + 2] = b;
            }
        }
        rgb
    }

    // ---- CUVID callbacks ----

    extern "C" fn sequence_callback(
        user_data: *mut c_void,
        video_format: *mut CUVIDEOFORMAT,
    ) -> i32 {
        let decoder = unsafe { &mut *(user_data as *mut NvdecAv1Decoder) };
        let format = unsafe { &*video_format };

        let output_format = if decoder.high_bitdepth {
            cudaVideoSurfaceFormat::cudaVideoSurfaceFormat_P016
        } else {
            cudaVideoSurfaceFormat::cudaVideoSurfaceFormat_NV12
        };

        let num_surfaces = (format.min_num_decode_surfaces as u64).max(8);

        let mut create_info: CUVIDDECODECREATEINFO = unsafe { std::mem::zeroed() };
        create_info.ulWidth = format.coded_width as u64;
        create_info.ulHeight = format.coded_height as u64;
        create_info.ulNumDecodeSurfaces = num_surfaces;
        create_info.CodecType = format.codec;
        create_info.ChromaFormat = format.chroma_format;
        create_info.ulCreationFlags = 0;
        create_info.OutputFormat = output_format;
        create_info.DeinterlaceMode =
            cudaVideoDeinterlaceMode::cudaVideoDeinterlaceMode_Adaptive;
        create_info.ulTargetWidth = format.coded_width as u64;
        create_info.ulTargetHeight = format.coded_height as u64;
        create_info.ulNumOutputSurfaces = 4;
        create_info.bitDepthMinus8 = format.bit_depth_luma_minus8 as u64;

        if !decoder.decoder.is_null() {
            let _ = unsafe { cuvidDestroyDecoder(decoder.decoder) };
            decoder.decoder = ptr::null_mut();
        }

        let result = unsafe { cuvidCreateDecoder(&mut decoder.decoder, &mut create_info) };
        if result != CUDA_SUCCESS {
            tracing::error!("Failed to create AV1 NVDEC decoder: {:?}", result);
            return 0;
        }

        decoder.width = format.coded_width;
        decoder.height = format.coded_height;

        let actual_bits = 8 + format.bit_depth_luma_minus8 as u8;
        if actual_bits > 8 {
            decoder.bit_depth = actual_bits;
        }

        tracing::info!(
            "[nvdec-av1] sequence: {}x{}, bit_depth={}, format={:?}",
            format.coded_width,
            format.coded_height,
            actual_bits,
            output_format,
        );

        num_surfaces as i32
    }

    extern "C" fn decode_callback(
        user_data: *mut c_void,
        pic_params: *mut CUVIDPICPARAMS,
    ) -> i32 {
        let decoder = unsafe { &mut *(user_data as *mut NvdecAv1Decoder) };
        if decoder.decoder.is_null() {
            return 0;
        }
        let result = unsafe { cuvidDecodePicture(decoder.decoder, pic_params) };
        if result != CUDA_SUCCESS {
            tracing::error!("Failed to decode AV1 picture: {:?}", result);
            return 0;
        }
        1
    }

    extern "C" fn display_callback(
        user_data: *mut c_void,
        disp_info: *mut CUVIDPARSERDISPINFO,
    ) -> i32 {
        let decoder = unsafe { &mut *(user_data as *mut NvdecAv1Decoder) };
        let info = unsafe { &*disp_info };

        if decoder.decoder.is_null() || info.picture_index < 0 {
            return 0;
        }

        let mut proc_params: CUVIDPROCPARAMS = unsafe { std::mem::zeroed() };
        proc_params.progressive_frame = info.progressive_frame as i32;

        let mut dev_ptr: u64 = 0;
        let mut pitch: u32 = 0;

        let result = unsafe {
            cuvidMapVideoFrame64(
                decoder.decoder,
                info.picture_index,
                &mut dev_ptr,
                &mut pitch,
                &mut proc_params,
            )
        };
        if result != CUDA_SUCCESS {
            tracing::error!("Failed to map AV1 video frame: {:?}", result);
            return 0;
        }

        let width = decoder.width as usize;
        let height = decoder.height as usize;
        let bytes_per_pixel = if decoder.bit_depth > 8 { 2 } else { 1 };
        let row_bytes = width * bytes_per_pixel;

        let total_size = row_bytes * height + row_bytes * (height / 2);
        let mut frame_data = vec![0u8; total_size];

        // Copy Y plane row by row (GPU pitch may differ from width)
        for y in 0..height {
            let src = (dev_ptr + (y as u64) * (pitch as u64))
                as cudarc::driver::sys::CUdeviceptr;
            let dst = unsafe { frame_data.as_mut_ptr().add(y * row_bytes) as *mut c_void };
            let r = unsafe { cudarc::driver::sys::cuMemcpyDtoH_v2(dst, src, row_bytes) };
            if r != CUDA_SUCCESS {
                tracing::error!("[nvdec-av1] Y row {} copy failed: {:?}", y, r);
                break;
            }
        }

        // Copy UV plane
        let uv_offset = row_bytes * height;
        let uv_height = height / 2;
        for y in 0..uv_height {
            let src = (dev_ptr + ((height + y) as u64) * (pitch as u64))
                as cudarc::driver::sys::CUdeviceptr;
            let dst = unsafe { frame_data.as_mut_ptr().add(uv_offset + y * row_bytes) as *mut c_void };
            let r = unsafe { cudarc::driver::sys::cuMemcpyDtoH_v2(dst, src, row_bytes) };
            if r != CUDA_SUCCESS {
                tracing::error!("[nvdec-av1] UV row {} copy failed: {:?}", y, r);
                break;
            }
        }

        let _ = unsafe { cuvidUnmapVideoFrame64(decoder.decoder, dev_ptr) };

        decoder.decoded_frames.push(NvdecFrame {
            data: frame_data,
            coded_width: decoder.width,
            coded_height: decoder.height,
            bit_depth: decoder.bit_depth,
        });

        1
    }
}

impl Drop for NvdecAv1Decoder {
    fn drop(&mut self) {
        if !self.parser.is_null() {
            let _ = unsafe { cuvidDestroyVideoParser(self.parser) };
        }
        if !self.decoder.is_null() {
            let _ = unsafe { cuvidDestroyDecoder(self.decoder) };
        }
    }
}

unsafe impl Send for NvdecAv1Decoder {}

/// Convert P016 Y-plane u16 values to depth in millimeters.
///
/// P016 stores 10-bit values MSB-aligned in u16: `val = gray10 << 6`.
/// The server encodes depth as: `gray10 = min(depth_mm >> depth_shift, 1023)`.
/// So: `depth_mm = (val >> 6) << depth_shift`.
pub fn p016_y_to_depth_mm(y_data: &[u8], depth_shift: u32) -> Vec<u16> {
    let pixel_count = y_data.len() / 2;
    let mut depth = Vec::with_capacity(pixel_count);
    for i in 0..pixel_count {
        let val = u16::from_le_bytes([y_data[i * 2], y_data[i * 2 + 1]]);
        let gray10 = val >> 6; // undo MSB alignment
        let mm = (gray10 as u32) << depth_shift;
        depth.push(mm as u16);
    }
    depth
}

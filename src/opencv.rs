//! OpenCV-compatible camera client for remote cameras.
//!
//! Supports both iroh P2P and MoQ relay transports.
//! Automatically negotiates H.264 (with NVDEC hardware decode) or JPEG encoding.
//!
//! # Example
//!
//! ```rust,no_run
//! use xoq::opencv::{CameraClient, CameraClientBuilder};
//!
//! #[tokio::main]
//! async fn main() {
//!     // Using iroh (P2P) - auto-negotiates H.264 or JPEG
//!     let mut client = CameraClient::connect("server-id-here").await.unwrap();
//!
//!     // Using MoQ (relay)
//!     let mut client = CameraClientBuilder::new()
//!         .moq("anon/my-camera")
//!         .connect()
//!         .await
//!         .unwrap();
//!
//!     loop {
//!         let frame = client.read_frame().await.unwrap();
//!         println!("Got frame: {}x{}", frame.width, frame.height);
//!     }
//! }
//! ```

use crate::frame::Frame;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::Mutex;

// ALPN protocols in preference order
const CAMERA_ALPN_H264: &[u8] = b"xoq/camera-h264/0";
const CAMERA_ALPN_JPEG: &[u8] = b"xoq/camera-jpeg/0";
const CAMERA_ALPN: &[u8] = b"xoq/camera/0"; // Legacy

/// Stream encoding type
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StreamEncoding {
    /// JPEG frames
    Jpeg,
    /// H.264 NAL units
    H264,
}

/// Transport type for camera client.
#[derive(Clone)]
pub enum Transport {
    /// Iroh P2P connection
    Iroh { server_id: String },
    /// MoQ relay connection
    Moq {
        path: String,
        relay_url: Option<String>,
    },
}

/// Builder for creating a camera client.
pub struct CameraClientBuilder {
    transport: Option<Transport>,
    prefer_h264: bool,
}

impl CameraClientBuilder {
    /// Create a new camera client builder.
    pub fn new() -> Self {
        Self {
            transport: None,
            prefer_h264: true, // Prefer H.264 by default if available
        }
    }

    /// Use iroh P2P transport.
    pub fn iroh(mut self, server_id: &str) -> Self {
        self.transport = Some(Transport::Iroh {
            server_id: server_id.to_string(),
        });
        self
    }

    /// Use MoQ relay transport.
    pub fn moq(mut self, path: &str) -> Self {
        self.transport = Some(Transport::Moq {
            path: path.to_string(),
            relay_url: None,
        });
        self
    }

    /// Use MoQ relay transport with custom relay URL.
    pub fn moq_with_relay(mut self, path: &str, relay_url: &str) -> Self {
        self.transport = Some(Transport::Moq {
            path: path.to_string(),
            relay_url: Some(relay_url.to_string()),
        });
        self
    }

    /// Prefer H.264 encoding (default: true).
    /// If false, will only try JPEG.
    pub fn prefer_h264(mut self, prefer: bool) -> Self {
        self.prefer_h264 = prefer;
        self
    }

    /// Force JPEG only (no H.264 negotiation).
    pub fn jpeg_only(mut self) -> Self {
        self.prefer_h264 = false;
        self
    }

    /// Connect to the camera server.
    pub async fn connect(self) -> Result<CameraClient> {
        let transport = self
            .transport
            .ok_or_else(|| anyhow::anyhow!("Transport not specified"))?;

        let prefer_h264 = self.prefer_h264;

        let inner = match transport {
            Transport::Iroh { server_id } => {
                Self::connect_iroh_inner(&server_id, prefer_h264).await?
            }
            Transport::Moq { path, relay_url } => {
                Self::connect_moq_inner(&path, relay_url.as_deref()).await?
            }
        };

        Ok(CameraClient { inner })
    }

    async fn connect_iroh_inner(server_id: &str, prefer_h264: bool) -> Result<CameraClientInner> {
        use crate::iroh::IrohClientBuilder;

        // Try ALPNs in order of preference
        let alpns: Vec<&[u8]> = if prefer_h264 {
            vec![CAMERA_ALPN_H264, CAMERA_ALPN_JPEG, CAMERA_ALPN]
        } else {
            vec![CAMERA_ALPN_JPEG, CAMERA_ALPN]
        };

        let mut last_error = None;
        for alpn in &alpns {
            match IrohClientBuilder::new()
                .alpn(alpn)
                .connect_str(server_id)
                .await
            {
                Ok(conn) => {
                    let encoding = if *alpn == CAMERA_ALPN_H264 {
                        StreamEncoding::H264
                    } else {
                        StreamEncoding::Jpeg
                    };

                    tracing::info!(
                        "Connected with {} encoding",
                        if encoding == StreamEncoding::H264 { "H.264" } else { "JPEG" }
                    );

                    let stream = conn.open_stream().await?;
                    let (_send, recv) = stream.split();

                    // Create decoder if H.264
                    #[cfg(feature = "nvenc")]
                    let decoder = if encoding == StreamEncoding::H264 {
                        Some(Arc::new(Mutex::new(NvdecDecoder::new()?)))
                    } else {
                        None
                    };

                    #[cfg(not(feature = "nvenc"))]
                    let decoder: Option<()> = None;

                    return Ok(CameraClientInner::Iroh {
                        recv: Arc::new(Mutex::new(recv)),
                        _conn: conn,
                        encoding,
                        #[cfg(feature = "nvenc")]
                        decoder,
                        #[cfg(not(feature = "nvenc"))]
                        _decoder: decoder,
                    });
                }
                Err(e) => {
                    tracing::debug!("Failed to connect with ALPN {:?}: {}",
                        String::from_utf8_lossy(alpn), e);
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("No ALPN protocols available")))
    }

    async fn connect_moq_inner(path: &str, relay_url: Option<&str>) -> Result<CameraClientInner> {
        use crate::moq::MoqBuilder;

        let mut builder = MoqBuilder::new().path(path);
        if let Some(url) = relay_url {
            builder = builder.relay(url);
        }
        let mut conn = builder.connect_subscriber().await?;

        // Wait for the camera track
        let track = conn
            .subscribe_track("camera")
            .await?
            .ok_or_else(|| anyhow::anyhow!("Camera track not found"))?;

        // MoQ currently only supports JPEG
        Ok(CameraClientInner::Moq {
            track,
            _conn: conn,
            encoding: StreamEncoding::Jpeg,
        })
    }
}

impl Default for CameraClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

enum CameraClientInner {
    Iroh {
        recv: Arc<Mutex<iroh::endpoint::RecvStream>>,
        _conn: crate::iroh::IrohConnection,
        encoding: StreamEncoding,
        #[cfg(feature = "nvenc")]
        decoder: Option<Arc<Mutex<NvdecDecoder>>>,
        #[cfg(not(feature = "nvenc"))]
        _decoder: Option<()>,
    },
    Moq {
        track: crate::moq::MoqTrackReader,
        _conn: crate::moq::MoqSubscriber,
        encoding: StreamEncoding,
    },
}

/// A client that receives camera frames from a remote server.
pub struct CameraClient {
    inner: CameraClientInner,
}

impl CameraClient {
    /// Connect to a remote camera server using iroh (legacy API).
    pub async fn connect(server_id: &str) -> Result<Self> {
        CameraClientBuilder::new().iroh(server_id).connect().await
    }

    /// Get the stream encoding type.
    pub fn encoding(&self) -> StreamEncoding {
        match &self.inner {
            CameraClientInner::Iroh { encoding, .. } => *encoding,
            CameraClientInner::Moq { encoding, .. } => *encoding,
        }
    }

    /// Request and read a single frame from the server.
    pub async fn read_frame(&mut self) -> Result<Frame> {
        match &mut self.inner {
            CameraClientInner::Iroh {
                recv,
                encoding,
                #[cfg(feature = "nvenc")]
                decoder,
                ..
            } => {
                // Read frame header and data
                let (width, height, timestamp, data) = {
                    let mut recv = recv.lock().await;

                    let mut header = [0u8; 20];
                    recv.read_exact(&mut header).await?;

                    let width = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
                    let height = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
                    let timestamp = u64::from_le_bytes([
                        header[8], header[9], header[10], header[11], header[12], header[13],
                        header[14], header[15],
                    ]);
                    let length =
                        u32::from_le_bytes([header[16], header[17], header[18], header[19]]);

                    let mut data = vec![0u8; length as usize];
                    recv.read_exact(&mut data).await?;

                    (width, height, timestamp, data)
                };

                // Decode based on encoding
                let frame = match encoding {
                    StreamEncoding::Jpeg => {
                        let mut frame = Frame::from_jpeg(&data)?;
                        frame.timestamp_us = timestamp;
                        frame
                    }
                    StreamEncoding::H264 => {
                        #[cfg(feature = "nvenc")]
                        {
                            if let Some(decoder) = decoder {
                                let mut dec = decoder.lock().await;
                                dec.decode(&data, width, height, timestamp)?
                            } else {
                                anyhow::bail!("H.264 stream but no decoder available");
                            }
                        }
                        #[cfg(not(feature = "nvenc"))]
                        {
                            anyhow::bail!("H.264 decoding requires nvenc feature");
                        }
                    }
                };

                // Verify dimensions match
                if frame.width != width || frame.height != height {
                    tracing::warn!(
                        "Frame dimension mismatch: expected {}x{}, got {}x{}",
                        width,
                        height,
                        frame.width,
                        frame.height
                    );
                }

                Ok(frame)
            }
            #[cfg(not(feature = "nvenc"))]
            CameraClientInner::Iroh {
                recv,
                encoding,
                ..
            } => {
                // Read frame header and data
                let (width, height, timestamp, data) = {
                    let mut recv = recv.lock().await;

                    let mut header = [0u8; 20];
                    recv.read_exact(&mut header).await?;

                    let width = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
                    let height = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
                    let timestamp = u64::from_le_bytes([
                        header[8], header[9], header[10], header[11], header[12], header[13],
                        header[14], header[15],
                    ]);
                    let length =
                        u32::from_le_bytes([header[16], header[17], header[18], header[19]]);

                    let mut data = vec![0u8; length as usize];
                    recv.read_exact(&mut data).await?;

                    (width, height, timestamp, data)
                };

                match encoding {
                    StreamEncoding::Jpeg => {
                        let mut frame = Frame::from_jpeg(&data)?;
                        frame.timestamp_us = timestamp;

                        if frame.width != width || frame.height != height {
                            tracing::warn!(
                                "Frame dimension mismatch: expected {}x{}, got {}x{}",
                                width, height, frame.width, frame.height
                            );
                        }

                        Ok(frame)
                    }
                    StreamEncoding::H264 => {
                        anyhow::bail!("H.264 decoding requires nvenc feature");
                    }
                }
            }
            CameraClientInner::Moq { track, encoding, .. } => {
                // Read frame from MoQ track with retry logic
                let mut retries = 0;
                let data = loop {
                    match track.read().await? {
                        Some(data) => break data,
                        None => {
                            retries += 1;
                            if retries > 200 {
                                anyhow::bail!("No frame available after retries");
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                        }
                    }
                };

                if data.len() < 12 {
                    anyhow::bail!("Invalid frame data");
                }

                let width = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                let height = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
                let timestamp = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
                let frame_data = &data[12..];

                let frame = match encoding {
                    StreamEncoding::Jpeg => {
                        let mut frame = Frame::from_jpeg(frame_data)?;
                        frame.timestamp_us = timestamp as u64;
                        frame
                    }
                    StreamEncoding::H264 => {
                        anyhow::bail!("H.264 over MoQ not yet supported");
                    }
                };

                if frame.width != width || frame.height != height {
                    tracing::warn!(
                        "Frame dimension mismatch: expected {}x{}, got {}x{}",
                        width,
                        height,
                        frame.width,
                        frame.height
                    );
                }

                Ok(frame)
            }
        }
    }

    /// Read frames continuously, calling the callback for each frame.
    pub async fn read_frames<F>(&mut self, mut callback: F) -> Result<()>
    where
        F: FnMut(Frame) -> bool,
    {
        loop {
            let frame = self.read_frame().await?;
            if !callback(frame) {
                break;
            }
        }
        Ok(())
    }
}

// ============================================================================
// NVDEC Hardware Decoder
// ============================================================================

#[cfg(feature = "nvenc")]
mod nvdec {
    use super::*;
    use cudarc::driver::CudaContext;
    use cudarc::driver::sys::CUresult;
    use nvidia_video_codec_sdk::sys::cuviddec::*;
    use nvidia_video_codec_sdk::sys::nvcuvid::*;
    use std::ffi::c_void;
    use std::ptr;

    // CUDA_SUCCESS = 0
    const CUDA_SUCCESS: CUresult = CUresult::CUDA_SUCCESS;

    /// NVDEC hardware H.264 decoder
    pub struct NvdecDecoder {
        parser: CUvideoparser,
        decoder: CUvideodecoder,
        _ctx: std::sync::Arc<CudaContext>,
        width: u32,
        height: u32,
        // Decoded frame storage
        decoded_frames: Vec<DecodedFrame>,
        // NV12 to RGB conversion buffer
        rgb_buffer: Vec<u8>,
    }

    struct DecodedFrame {
        data: Vec<u8>,
        width: u32,
        height: u32,
        timestamp: u64,
    }

    impl NvdecDecoder {
        pub fn new() -> Result<Self> {
            let ctx = CudaContext::new(0)
                .map_err(|e| anyhow::anyhow!("Failed to create CUDA context: {}", e))?;

            Ok(NvdecDecoder {
                parser: ptr::null_mut(),
                decoder: ptr::null_mut(),
                _ctx: ctx,
                width: 0,
                height: 0,
                decoded_frames: Vec::new(),
                rgb_buffer: Vec::new(),
            })
        }

        fn ensure_decoder(&mut self, width: u32, height: u32) -> Result<()> {
            if self.width == width && self.height == height && !self.parser.is_null() {
                return Ok(());
            }

            // Destroy old decoder/parser if exists
            self.destroy_decoder();

            self.width = width;
            self.height = height;

            // Create video parser
            let mut parser_params: CUVIDPARSERPARAMS = unsafe { std::mem::zeroed() };
            parser_params.CodecType = cudaVideoCodec::cudaVideoCodec_H264;
            parser_params.ulMaxNumDecodeSurfaces = 4;
            parser_params.ulMaxDisplayDelay = 0; // Low latency
            parser_params.pUserData = self as *mut _ as *mut c_void;
            parser_params.pfnSequenceCallback = Some(Self::sequence_callback);
            parser_params.pfnDecodePicture = Some(Self::decode_callback);
            parser_params.pfnDisplayPicture = Some(Self::display_callback);

            let result = unsafe { cuvidCreateVideoParser(&mut self.parser, &mut parser_params) };
            if result != CUDA_SUCCESS {
                anyhow::bail!("Failed to create video parser: {:?}", result);
            }

            Ok(())
        }

        fn destroy_decoder(&mut self) {
            if !self.parser.is_null() {
                let _ = unsafe { cuvidDestroyVideoParser(self.parser) };
                self.parser = ptr::null_mut();
            }
            if !self.decoder.is_null() {
                let _ = unsafe { cuvidDestroyDecoder(self.decoder) };
                self.decoder = ptr::null_mut();
            }
        }

        pub fn decode(&mut self, h264_data: &[u8], width: u32, height: u32, timestamp: u64) -> Result<Frame> {
            self.ensure_decoder(width, height)?;

            // Create packet
            let mut packet: CUVIDSOURCEDATAPACKET = unsafe { std::mem::zeroed() };
            packet.payload = h264_data.as_ptr();
            packet.payload_size = h264_data.len() as u64;
            packet.timestamp = timestamp as i64;

            // Parse the data (this triggers callbacks)
            let result = unsafe { cuvidParseVideoData(self.parser, &mut packet) };
            if result != CUDA_SUCCESS {
                anyhow::bail!("Failed to parse video data: {:?}", result);
            }

            // Get decoded frame
            if let Some(decoded) = self.decoded_frames.pop() {
                // Convert NV12 to RGB
                self.nv12_to_rgb(&decoded.data, decoded.width, decoded.height);

                Ok(Frame {
                    width: decoded.width,
                    height: decoded.height,
                    data: self.rgb_buffer.clone(),
                    timestamp_us: decoded.timestamp,
                })
            } else {
                // No frame decoded yet - return placeholder frame
                // This can happen at stream start before first I-frame
                Ok(Frame {
                    width,
                    height,
                    data: vec![128u8; (width * height * 3) as usize], // Gray frame
                    timestamp_us: timestamp,
                })
            }
        }

        fn nv12_to_rgb(&mut self, nv12: &[u8], width: u32, height: u32) {
            let width = width as usize;
            let height = height as usize;
            let y_size = width * height;

            self.rgb_buffer.resize(width * height * 3, 0);

            for y in 0..height {
                for x in 0..width {
                    let y_val = nv12.get(y * width + x).copied().unwrap_or(0) as f32;

                    let uv_idx = y_size + (y / 2) * width + (x / 2) * 2;
                    let u = nv12.get(uv_idx).copied().unwrap_or(128) as f32;
                    let v = nv12.get(uv_idx + 1).copied().unwrap_or(128) as f32;

                    // YUV to RGB (BT.601)
                    let c = y_val - 16.0;
                    let d = u - 128.0;
                    let e = v - 128.0;

                    let r = (1.164 * c + 1.596 * e).clamp(0.0, 255.0) as u8;
                    let g = (1.164 * c - 0.392 * d - 0.813 * e).clamp(0.0, 255.0) as u8;
                    let b = (1.164 * c + 2.017 * d).clamp(0.0, 255.0) as u8;

                    let rgb_idx = (y * width + x) * 3;
                    self.rgb_buffer[rgb_idx] = r;
                    self.rgb_buffer[rgb_idx + 1] = g;
                    self.rgb_buffer[rgb_idx + 2] = b;
                }
            }
        }

        // Callback when sequence header is parsed - creates the actual decoder
        extern "C" fn sequence_callback(
            user_data: *mut c_void,
            video_format: *mut CUVIDEOFORMAT,
        ) -> i32 {
            let decoder = unsafe { &mut *(user_data as *mut NvdecDecoder) };
            let format = unsafe { &*video_format };

            // Create decoder with these parameters
            let mut create_info: CUVIDDECODECREATEINFO = unsafe { std::mem::zeroed() };
            create_info.ulWidth = format.coded_width as u64;
            create_info.ulHeight = format.coded_height as u64;
            create_info.ulNumDecodeSurfaces = 4;
            create_info.CodecType = format.codec;
            create_info.ChromaFormat = format.chroma_format;
            create_info.ulCreationFlags = 0;
            create_info.OutputFormat = cudaVideoSurfaceFormat::cudaVideoSurfaceFormat_NV12;
            create_info.DeinterlaceMode = cudaVideoDeinterlaceMode::cudaVideoDeinterlaceMode_Adaptive;
            create_info.ulTargetWidth = format.coded_width as u64;
            create_info.ulTargetHeight = format.coded_height as u64;
            create_info.ulNumOutputSurfaces = 2;

            if !decoder.decoder.is_null() {
                let _ = unsafe { cuvidDestroyDecoder(decoder.decoder) };
                decoder.decoder = ptr::null_mut();
            }

            let result = unsafe { cuvidCreateDecoder(&mut decoder.decoder, &mut create_info) };
            if result != CUDA_SUCCESS {
                tracing::error!("Failed to create decoder: {:?}", result);
                return 0;
            }

            decoder.width = format.coded_width;
            decoder.height = format.coded_height;

            format.min_num_decode_surfaces as i32
        }

        // Callback when a picture is ready to decode
        extern "C" fn decode_callback(
            user_data: *mut c_void,
            pic_params: *mut CUVIDPICPARAMS,
        ) -> i32 {
            let decoder = unsafe { &mut *(user_data as *mut NvdecDecoder) };

            if decoder.decoder.is_null() {
                return 0;
            }

            let result = unsafe { cuvidDecodePicture(decoder.decoder, pic_params) };
            if result != CUDA_SUCCESS {
                tracing::error!("Failed to decode picture: {:?}", result);
                return 0;
            }

            1
        }

        // Callback when a picture is ready to display
        extern "C" fn display_callback(
            user_data: *mut c_void,
            disp_info: *mut CUVIDPARSERDISPINFO,
        ) -> i32 {
            let decoder = unsafe { &mut *(user_data as *mut NvdecDecoder) };
            let info = unsafe { &*disp_info };

            if decoder.decoder.is_null() || info.picture_index < 0 {
                return 0;
            }

            // Map the video frame
            let mut proc_params: CUVIDPROCPARAMS = unsafe { std::mem::zeroed() };
            proc_params.progressive_frame = info.progressive_frame as i32;
            proc_params.top_field_first = info.top_field_first as i32;

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
                tracing::error!("Failed to map video frame: {:?}", result);
                return 0;
            }

            // Copy NV12 data from GPU to CPU
            let width = decoder.width as usize;
            let height = decoder.height as usize;
            let nv12_size = width * height * 3 / 2;
            let mut nv12_data = vec![0u8; nv12_size];

            // Note: dev_ptr is a device pointer, we need cuMemcpy to copy from GPU
            // For now, this is a simplified version - proper implementation would use
            // cuMemcpyDtoH or cuMemcpy2D
            // TODO: Use proper CUDA memory copy

            // Copy Y plane (row by row due to pitch)
            for y in 0..height {
                let src_offset = y * pitch as usize;
                let dst_offset = y * width;
                unsafe {
                    cudarc::driver::sys::cuMemcpyDtoH_v2(
                        nv12_data.as_mut_ptr().add(dst_offset) as *mut c_void,
                        (dev_ptr + src_offset as u64) as cudarc::driver::sys::CUdeviceptr,
                        width,
                    );
                }
            }

            // Copy UV plane
            let uv_height = height / 2;
            for y in 0..uv_height {
                let src_offset = (height * pitch as usize) + y * pitch as usize;
                let dst_offset = width * height + y * width;
                unsafe {
                    cudarc::driver::sys::cuMemcpyDtoH_v2(
                        nv12_data.as_mut_ptr().add(dst_offset) as *mut c_void,
                        (dev_ptr + src_offset as u64) as cudarc::driver::sys::CUdeviceptr,
                        width,
                    );
                }
            }

            // Unmap
            let _ = unsafe { cuvidUnmapVideoFrame64(decoder.decoder, dev_ptr) };

            // Store decoded frame
            decoder.decoded_frames.push(DecodedFrame {
                data: nv12_data,
                width: decoder.width,
                height: decoder.height,
                timestamp: info.timestamp as u64,
            });

            1
        }
    }

    impl Drop for NvdecDecoder {
        fn drop(&mut self) {
            self.destroy_decoder();
        }
    }

    unsafe impl Send for NvdecDecoder {}
}

#[cfg(feature = "nvenc")]
pub use nvdec::NvdecDecoder;

// ============================================================================
// Legacy API
// ============================================================================

/// Builder for creating a remote camera connection (legacy).
pub struct RemoteCameraBuilder {
    server_id: String,
}

impl RemoteCameraBuilder {
    /// Create a new builder for connecting to a remote camera.
    pub fn new(server_id: &str) -> Self {
        RemoteCameraBuilder {
            server_id: server_id.to_string(),
        }
    }

    /// Connect to the remote camera server.
    pub async fn connect(self) -> Result<CameraClient> {
        CameraClient::connect(&self.server_id).await
    }
}

/// Create a builder for connecting to a remote camera (legacy).
pub fn remote_camera(server_id: &str) -> RemoteCameraBuilder {
    RemoteCameraBuilder::new(server_id)
}

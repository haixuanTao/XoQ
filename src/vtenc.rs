//! VideoToolbox H.264 encoder for macOS.
//!
//! Encodes BGRA or RGB frames to Annex B H.264 using Apple's hardware encoder.
//! This is the macOS equivalent of the NVENC encoder used on Linux.

use anyhow::Result;
use core_foundation_sys::base::OSStatus;
use core_media_sys::CMSampleBufferRef;
use std::sync::{mpsc, Arc, Mutex};
use video_toolbox_sys::codecs;
use video_toolbox_sys::compression::{
    VTCompressionSessionCompleteFrames, VTCompressionSessionEncodeFrame,
    VTCompressionSessionInvalidate, VTCompressionSessionRef,
};
use video_toolbox_sys::helpers::{
    create_pixel_buffer, CompressionSessionBuilder, NalExtractor, NalUnit, PixelBufferConfig,
    PixelBufferGuard,
};

/// An encoded frame containing structured NAL unit data.
///
/// Used by `encode_pixel_buffer_nals()` to provide raw NAL units
/// for CMAF muxing without the Annex B → parse → NAL roundtrip.
pub struct EncodedFrame {
    /// NAL units (slice data, excluding SPS/PPS).
    pub nals: Vec<NalUnit>,
    /// SPS data (present on keyframes).
    pub sps: Option<Vec<u8>>,
    /// PPS data (present on keyframes).
    pub pps: Option<Vec<u8>>,
    /// Whether this frame is a keyframe.
    pub is_keyframe: bool,
}

/// VideoToolbox H.264 encoder.
///
/// Encodes frames to Annex B H.264 NAL units using Apple's hardware VideoToolbox encoder.
/// Outputs the same wire format as NVENC: Annex B start codes (0x00000001) + NAL data.
pub struct VtEncoder {
    session: VTCompressionSessionRef,
    width: u32,
    height: u32,
    frame_count: u64,
    encoded_rx: mpsc::Receiver<EncodedFrame>,
    // Keep sender alive; the callback closure owns a clone via Arc<Mutex<>>
    _encoded_tx: Arc<Mutex<mpsc::Sender<EncodedFrame>>>,
}

// Safety: VtEncoder is used from a single async task via Mutex.
// The compression session callback runs on its own thread, but communicates
// only through the mpsc channel.
unsafe impl Send for VtEncoder {}

impl VtEncoder {
    /// Create a new VideoToolbox H.264 encoder.
    ///
    /// # Arguments
    /// * `width` - Frame width in pixels
    /// * `height` - Frame height in pixels
    /// * `fps` - Target framerate
    /// * `bitrate` - Target bitrate in bits per second
    pub fn new(width: u32, height: u32, fps: u32, bitrate: u32) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<EncodedFrame>();
        let tx_arc = Arc::new(Mutex::new(tx));
        let tx_for_callback = tx_arc.clone();

        // Build compression session with callback
        let session = CompressionSessionBuilder::new(
            width as i32,
            height as i32,
            codecs::video::H264,
        )
        .hardware_accelerated(true)
        .low_latency(true)
        .real_time(true)
        .bitrate(bitrate as i64)
        .frame_rate(fps as f64)
        .keyframe_interval(fps as i32)
        .profile_level(unsafe {
            video_toolbox_sys::compression::kVTProfileLevel_H264_High_AutoLevel
        })
        .build(move |_output_ref, _source_ref, status: OSStatus, _info_flags, sample_buffer_ptr| {
            if status != 0 || sample_buffer_ptr.is_null() {
                return;
            }

            let sample_buffer = sample_buffer_ptr as CMSampleBufferRef;
            let local_extractor = NalExtractor::new();

            unsafe {
                // Check if keyframe
                let is_keyframe = local_extractor.is_keyframe(sample_buffer);

                // Extract NAL units
                let nals = match local_extractor.extract_nal_units(sample_buffer) {
                    Ok(n) => n,
                    Err(_) => return,
                };

                // Extract SPS/PPS on keyframes
                let (sps, pps) = if is_keyframe {
                    if let Some(fmt_desc) = local_extractor.get_format_description(sample_buffer) {
                        if let Ok(params) = local_extractor.extract_parameter_sets(fmt_desc) {
                            (Some(params.sps), Some(params.pps))
                        } else {
                            (None, None)
                        }
                    } else {
                        (None, None)
                    }
                } else {
                    (None, None)
                };

                if !nals.is_empty() || sps.is_some() {
                    let frame = EncodedFrame {
                        nals,
                        sps,
                        pps,
                        is_keyframe,
                    };
                    if let Ok(guard) = tx_for_callback.lock() {
                        let _ = guard.send(frame);
                    }
                }
            }
        })
        .map_err(|status| anyhow::anyhow!("Failed to create VT compression session: OSStatus {}", status))?;

        Ok(VtEncoder {
            session,
            width,
            height,
            frame_count: 0,
            encoded_rx: rx,
            _encoded_tx: tx_arc,
        })
    }

    /// Encode a BGRA frame to Annex B H.264.
    ///
    /// # Arguments
    /// * `bgra` - BGRA pixel data (4 bytes per pixel, width * height * 4 total)
    /// * `timestamp_us` - Presentation timestamp in microseconds
    pub fn encode_bgra(&mut self, bgra: &[u8], timestamp_us: u64) -> Result<Vec<u8>> {
        let config = PixelBufferConfig::new(self.width as usize, self.height as usize);
        let pixel_buffer = create_pixel_buffer(&config)
            .map_err(|e| anyhow::anyhow!("Failed to create pixel buffer: CVReturn {}", e))?;

        unsafe {
            // Lock and copy BGRA data
            let guard = PixelBufferGuard::lock(pixel_buffer)
                .map_err(|e| anyhow::anyhow!("Failed to lock pixel buffer: CVReturn {}", e))?;

            let dst = guard.base_address();
            let dst_stride = guard.bytes_per_row();
            let src_stride = self.width as usize * 4;

            for y in 0..self.height as usize {
                let src_offset = y * src_stride;
                let dst_offset = y * dst_stride;
                if src_offset + src_stride <= bgra.len() {
                    std::ptr::copy_nonoverlapping(
                        bgra.as_ptr().add(src_offset),
                        dst.add(dst_offset),
                        src_stride,
                    );
                }
            }

            drop(guard); // Unlock before encoding

            // Create CMTime for presentation timestamp
            let pts = core_media_sys::CMTime {
                value: timestamp_us as i64,
                timescale: 1_000_000,
                flags: 1, // kCMTimeFlags_Valid
                epoch: 0,
            };
            let duration = core_media_sys::CMTime {
                value: 0,
                timescale: 0,
                flags: 0, // kCMTimeFlags_Invalid (let encoder decide)
                epoch: 0,
            };

            let status = VTCompressionSessionEncodeFrame(
                self.session,
                pixel_buffer,
                pts,
                duration,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );

            // Release pixel buffer
            core_foundation_sys::base::CFRelease(pixel_buffer as _);

            if status != 0 {
                anyhow::bail!("VTCompressionSessionEncodeFrame failed: OSStatus {}", status);
            }

            // Force synchronous output
            let complete_time = core_media_sys::CMTime {
                value: timestamp_us as i64,
                timescale: 1_000_000,
                flags: 1,
                epoch: 0,
            };
            VTCompressionSessionCompleteFrames(self.session, complete_time);
        }

        self.frame_count += 1;

        // Receive encoded frame from callback and convert to Annex B
        let encoded = self
            .encoded_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("Encoder callback channel closed"))?;

        Ok(Self::encoded_frame_to_annex_b(&encoded))
    }

    /// Convert an EncodedFrame to Annex B byte stream.
    fn encoded_frame_to_annex_b(frame: &EncodedFrame) -> Vec<u8> {
        let mut annex_b_data = Vec::new();

        // Prepend SPS/PPS on keyframes
        if let Some(ref sps) = frame.sps {
            annex_b_data.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
            annex_b_data.extend_from_slice(sps);
        }
        if let Some(ref pps) = frame.pps {
            annex_b_data.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
            annex_b_data.extend_from_slice(pps);
        }

        // Append all slice NALs
        for nal in &frame.nals {
            let ab = nal.to_annex_b();
            annex_b_data.extend_from_slice(&ab);
        }

        annex_b_data
    }

    /// Encode a retained CVPixelBuffer directly to Annex B H.264 (zero-copy).
    ///
    /// Passes the CVPixelBuffer straight to VideoToolbox without creating a new
    /// buffer or copying any pixel data. The pixel buffer must contain BGRA data
    /// matching the encoder's dimensions.
    ///
    /// # Arguments
    /// * `pixel_buffer_ptr` - Raw CVPixelBuffer pointer from `RetainedPixelBuffer::as_ptr()`
    /// * `timestamp_us` - Presentation timestamp in microseconds
    ///
    /// # Safety
    /// The pointer must be a valid, retained CVPixelBufferRef with BGRA pixel data.
    pub fn encode_pixel_buffer(
        &mut self,
        pixel_buffer_ptr: *const std::ffi::c_void,
        timestamp_us: u64,
    ) -> Result<Vec<u8>> {
        unsafe {
            let pts = core_media_sys::CMTime {
                value: timestamp_us as i64,
                timescale: 1_000_000,
                flags: 1, // kCMTimeFlags_Valid
                epoch: 0,
            };
            let duration = core_media_sys::CMTime {
                value: 0,
                timescale: 0,
                flags: 0, // kCMTimeFlags_Invalid (let encoder decide)
                epoch: 0,
            };

            // Cast to CVImageBufferRef — same underlying CoreFoundation type
            let cv_pixel_buffer =
                pixel_buffer_ptr as video_toolbox_sys::cv_types::CVImageBufferRef;

            let status = VTCompressionSessionEncodeFrame(
                self.session,
                cv_pixel_buffer,
                pts,
                duration,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );

            if status != 0 {
                anyhow::bail!(
                    "VTCompressionSessionEncodeFrame failed: OSStatus {}",
                    status
                );
            }

            // Force synchronous output
            let complete_time = core_media_sys::CMTime {
                value: timestamp_us as i64,
                timescale: 1_000_000,
                flags: 1,
                epoch: 0,
            };
            VTCompressionSessionCompleteFrames(self.session, complete_time);
        }

        self.frame_count += 1;

        // Receive encoded frame from callback and convert to Annex B
        let encoded = self
            .encoded_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("Encoder callback channel closed"))?;

        Ok(Self::encoded_frame_to_annex_b(&encoded))
    }

    /// Encode a retained CVPixelBuffer directly to structured NAL units (zero-copy).
    ///
    /// Returns an `EncodedFrame` with raw NAL units suitable for CMAF muxing,
    /// avoiding the NAL → Annex B → parse → NAL roundtrip.
    ///
    /// # Arguments
    /// * `pixel_buffer_ptr` - Raw CVPixelBuffer pointer from `RetainedPixelBuffer::as_ptr()`
    /// * `timestamp_us` - Presentation timestamp in microseconds
    ///
    /// # Safety
    /// The pointer must be a valid, retained CVPixelBufferRef with BGRA pixel data.
    pub fn encode_pixel_buffer_nals(
        &mut self,
        pixel_buffer_ptr: *const std::ffi::c_void,
        timestamp_us: u64,
    ) -> Result<EncodedFrame> {
        unsafe {
            let pts = core_media_sys::CMTime {
                value: timestamp_us as i64,
                timescale: 1_000_000,
                flags: 1, // kCMTimeFlags_Valid
                epoch: 0,
            };
            let duration = core_media_sys::CMTime {
                value: 0,
                timescale: 0,
                flags: 0, // kCMTimeFlags_Invalid (let encoder decide)
                epoch: 0,
            };

            let cv_pixel_buffer =
                pixel_buffer_ptr as video_toolbox_sys::cv_types::CVImageBufferRef;

            let status = VTCompressionSessionEncodeFrame(
                self.session,
                cv_pixel_buffer,
                pts,
                duration,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );

            if status != 0 {
                anyhow::bail!(
                    "VTCompressionSessionEncodeFrame failed: OSStatus {}",
                    status
                );
            }

            let complete_time = core_media_sys::CMTime {
                value: timestamp_us as i64,
                timescale: 1_000_000,
                flags: 1,
                epoch: 0,
            };
            VTCompressionSessionCompleteFrames(self.session, complete_time);
        }

        self.frame_count += 1;

        let encoded = self
            .encoded_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("Encoder callback channel closed"))?;

        Ok(encoded)
    }

    /// Encode an RGB frame to Annex B H.264.
    ///
    /// Converts RGB to BGRA internally, then encodes.
    ///
    /// # Arguments
    /// * `rgb` - RGB pixel data (3 bytes per pixel, width * height * 3 total)
    /// * `timestamp_us` - Presentation timestamp in microseconds
    pub fn encode_rgb(&mut self, rgb: &[u8], timestamp_us: u64) -> Result<Vec<u8>> {
        let pixel_count = (self.width * self.height) as usize;
        let mut bgra = vec![0u8; pixel_count * 4];

        for i in 0..pixel_count {
            let rgb_idx = i * 3;
            let bgra_idx = i * 4;
            if rgb_idx + 2 < rgb.len() {
                bgra[bgra_idx] = rgb[rgb_idx + 2];     // B
                bgra[bgra_idx + 1] = rgb[rgb_idx + 1]; // G
                bgra[bgra_idx + 2] = rgb[rgb_idx];     // R
                bgra[bgra_idx + 3] = 255;              // A
            }
        }

        self.encode_bgra(&bgra, timestamp_us)
    }
}

impl Drop for VtEncoder {
    fn drop(&mut self) {
        if !self.session.is_null() {
            unsafe {
                VTCompressionSessionInvalidate(self.session);
            }
        }
    }
}

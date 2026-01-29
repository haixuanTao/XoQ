//! Hardware-accelerated video codec library for xoq.
//!
//! This crate provides hardware-accelerated video encoding and decoding
//! capabilities for the xoq robotics communication framework. It supports multiple
//! backends:
//!
//! - **NVIDIA NVENC/NVDEC** (`nvidia` feature): Hardware encoding/decoding on NVIDIA GPUs
//! - **Apple VideoToolbox** (`apple` feature): Future support for Apple Silicon
//! - **CPU fallback** (`cpu` feature): Future software encoding fallback
//!
//! # Quick Start (Encoding)
//!
//! ```ignore
//! use xoq_codec::{VideoEncoder, NvencEncoder, EncoderConfig, Codec, VideoFrame};
//!
//! // Create encoder configuration
//! let config = EncoderConfig::new(1920, 1080)
//!     .codec(Codec::H264)
//!     .framerate(30, 1)
//!     .for_low_latency();
//!
//! // Create NVIDIA encoder
//! let mut encoder = NvencEncoder::new(config)?;
//!
//! // Encode a frame
//! let frame = VideoFrame::from_rgb(1920, 1080, rgb_data, timestamp_us);
//! let packet = encoder.encode(&frame)?;
//!
//! // packet.data contains H.264 NAL units ready for streaming
//! ```
//!
//! # Quick Start (Decoding)
//!
//! ```ignore
//! use xoq_codec::{VideoDecoder, NvdecDecoder, DecoderConfig, Codec};
//!
//! // Create decoder configuration
//! let config = DecoderConfig::new(Codec::H264)
//!     .for_low_latency();
//!
//! // Create NVIDIA decoder
//! let mut decoder = NvdecDecoder::new(config)?;
//!
//! // Decode a packet
//! if let Some(frame) = decoder.decode(&packet)? {
//!     // frame.data contains decoded pixels in NV12 format
//! }
//! ```
//!
//! # Feature Flags
//!
//! - `nvidia` - Enable NVIDIA NVENC/NVDEC hardware encoding/decoding (requires CUDA)
//! - `xoq-frame` - Enable conversion to/from xoq::Frame type
//! - `h264`, `hevc`, `av1` - Codec-specific features (informational)
//!
//! # Architecture
//!
//! The crate is built around the [`VideoEncoder`] and [`VideoDecoder`] traits,
//! which provide unified interfaces for all encoder/decoder backends. Each backend
//! implements these traits, allowing code to be written generically.

mod error;
mod frame;
mod traits;
mod types;

pub use error::CodecError;
pub use frame::{VideoFrame, expected_frame_size};
pub use traits::{
    DecodedFrame, EncodeParams, EncodedPacket, VideoDecoder, VideoEncoder, VideoFrameData,
};
pub use types::{
    Codec, DecoderConfig, EncoderConfig, EncoderPreset, PixelFormat, RateControlMode, TuningMode,
};

// NVIDIA backend
#[cfg(feature = "nvidia")]
pub mod nvidia;

#[cfg(feature = "nvidia")]
pub use nvidia::NvencEncoder;

#[cfg(feature = "nvidia")]
pub use nvidia::NvdecDecoder;

// Re-export xoq::Frame when the feature is enabled
#[cfg(feature = "xoq-frame")]
pub use xoq::Frame as XoqFrame;

//! NVIDIA NVENC/NVDEC hardware codec implementation.
//!
//! This module provides hardware-accelerated video encoding and decoding using
//! NVIDIA's NVENC and NVDEC technologies, available on GeForce, Quadro, and Tesla GPUs.
//!
//! # Requirements
//!
//! - NVIDIA GPU with NVENC/NVDEC support (Maxwell or newer architecture)
//! - NVIDIA driver installed
//! - CUDA toolkit (optional, for CUDA context management)
//!
//! # Encoding Example
//!
//! ```ignore
//! use xoq_codec::{NvencEncoder, EncoderConfig, Codec, VideoFrame};
//!
//! let config = EncoderConfig::new(1920, 1080)
//!     .codec(Codec::H264)
//!     .for_low_latency();
//!
//! let mut encoder = NvencEncoder::new(config)?;
//!
//! let frame = VideoFrame::from_rgb(1920, 1080, rgb_data, 0);
//! let packet = encoder.encode(&frame)?;
//! ```
//!
//! # Decoding Example
//!
//! ```ignore
//! use xoq_codec::{NvdecDecoder, DecoderConfig, Codec};
//!
//! let config = DecoderConfig::new(Codec::H264)
//!     .for_low_latency();
//!
//! let mut decoder = NvdecDecoder::new(config)?;
//!
//! // Decode a packet
//! if let Some(frame) = decoder.decode(&packet)? {
//!     // frame.data contains decoded NV12 pixels
//! }
//! ```

mod api;
mod decoder;
mod encoder;

pub use decoder::NvdecDecoder;
pub use encoder::NvencEncoder;

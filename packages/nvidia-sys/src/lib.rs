//! Raw NVIDIA Video Codec SDK bindings.
//!
//! This crate provides low-level bindings to the NVIDIA Video Codec SDK,
//! including both the NVENC (encoder) and NVDEC/CUVID (decoder) APIs.
//! These bindings are based on the official NVIDIA headers.
//!
//! For a higher-level, safe API, use the `xoq-codec` crate instead.
//!
//! ## Modules
//!
//! - [`nvEncodeAPI`] - NVENC hardware encoding API
//! - [`cuviddec`] - CUVID/NVDEC hardware decoding API
//! - [`nvcuvid`] - CUVID video parser API
//! - [`guid`] - GUIDs for codecs, profiles, and presets
//! - [`version`] - API version constants

#![allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    clippy::all,
    dead_code
)]

// Encoder bindings
pub mod nvEncodeAPI;
pub mod guid;
pub mod version;

// Decoder bindings
pub mod cuviddec;
pub mod nvcuvid;

// Re-export commonly used items from encoder
pub use guid::*;
pub use version::*;
pub use nvEncodeAPI::*;

// Re-export commonly used items from decoder
pub use cuviddec::*;
pub use nvcuvid::*;

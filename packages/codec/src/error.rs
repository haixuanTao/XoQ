//! Error types for video codec operations.

use thiserror::Error;

/// Errors that can occur during video encoding/decoding.
#[derive(Debug, Error)]
pub enum CodecError {
    /// No encode-capable device was detected.
    #[error("no encode-capable device detected")]
    NoEncodeDevice,

    /// The device is not supported for encoding.
    #[error("device not supported for encoding")]
    UnsupportedDevice,

    /// Invalid encoder device.
    #[error("invalid encoder device")]
    InvalidEncoderDevice,

    /// Invalid device.
    #[error("invalid device")]
    InvalidDevice,

    /// Device no longer exists.
    #[error("device no longer exists")]
    DeviceNotExist,

    /// Invalid pointer passed to API.
    #[error("invalid pointer")]
    InvalidPtr,

    /// Invalid parameter passed to API.
    #[error("invalid parameter: {0}")]
    InvalidParam(String),

    /// API call made in wrong sequence.
    #[error("invalid API call sequence")]
    InvalidCall,

    /// Out of memory.
    #[error("out of memory")]
    OutOfMemory,

    /// Encoder not initialized.
    #[error("encoder not initialized")]
    EncoderNotInitialized,

    /// Unsupported parameter.
    #[error("unsupported parameter: {0}")]
    UnsupportedParam(String),

    /// Lock is busy (non-blocking call).
    #[error("lock is busy")]
    LockBusy,

    /// Buffer is not large enough.
    #[error("buffer not large enough")]
    NotEnoughBuffer,

    /// Invalid API version.
    #[error("invalid API version")]
    InvalidVersion,

    /// Failed to map input resource.
    #[error("failed to map input resource")]
    MapFailed,

    /// Encoder needs more input frames.
    #[error("encoder needs more input")]
    NeedMoreInput,

    /// Encoder is busy.
    #[error("encoder is busy")]
    EncoderBusy,

    /// Generic/unknown error.
    #[error("encoder error: {0}")]
    Generic(String),

    /// Feature not implemented.
    #[error("feature not implemented: {0}")]
    Unimplemented(String),

    /// Failed to register resource.
    #[error("failed to register resource")]
    ResourceRegisterFailed,

    /// Resource not registered.
    #[error("resource not registered")]
    ResourceNotRegistered,

    /// Resource not mapped.
    #[error("resource not mapped")]
    ResourceNotMapped,

    /// Encoder needs more output buffers.
    #[error("encoder needs more output")]
    NeedMoreOutput,

    /// CUDA error.
    #[error("CUDA error: {0}")]
    CudaError(String),

    /// Unsupported codec.
    #[error("unsupported codec")]
    UnsupportedCodec,

    /// Unsupported pixel format.
    #[error("unsupported pixel format")]
    UnsupportedPixelFormat,

    /// Invalid frame dimensions.
    #[error("invalid frame dimensions: {width}x{height}")]
    InvalidDimensions { width: u32, height: u32 },

    /// Frame conversion error.
    #[error("frame conversion error: {0}")]
    ConversionError(String),
}

impl CodecError {
    /// Create an InvalidParam error with a message.
    pub fn invalid_param(msg: impl Into<String>) -> Self {
        Self::InvalidParam(msg.into())
    }

    /// Create an UnsupportedParam error with a message.
    pub fn unsupported_param(msg: impl Into<String>) -> Self {
        Self::UnsupportedParam(msg.into())
    }

    /// Create a Generic error with a message.
    pub fn generic(msg: impl Into<String>) -> Self {
        Self::Generic(msg.into())
    }

    /// Create an Unimplemented error with a message.
    pub fn unimplemented(msg: impl Into<String>) -> Self {
        Self::Unimplemented(msg.into())
    }
}

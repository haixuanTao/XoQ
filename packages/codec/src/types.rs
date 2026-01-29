//! Core types for video encoding/decoding.

/// Video codec type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Codec {
    /// H.264/AVC codec.
    H264,
    /// H.265/HEVC codec.
    Hevc,
    /// AV1 codec.
    Av1,
}

impl Default for Codec {
    fn default() -> Self {
        Self::H264
    }
}

/// Pixel format for video frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// RGB with 8 bits per channel (24 bits per pixel).
    Rgb,
    /// RGBA with 8 bits per channel (32 bits per pixel).
    Rgba,
    /// BGR with 8 bits per channel (24 bits per pixel).
    Bgr,
    /// BGRA with 8 bits per channel (32 bits per pixel).
    Bgra,
    /// NV12 (YUV 4:2:0, planar Y + interleaved UV).
    Nv12,
    /// I420/YUV420P (YUV 4:2:0, planar Y + U + V).
    I420,
    /// ARGB with 8 bits per channel (32 bits per pixel).
    Argb,
    /// ABGR with 8 bits per channel (32 bits per pixel).
    Abgr,
}

impl PixelFormat {
    /// Returns the number of bytes per pixel for packed formats,
    /// or an approximation for planar formats.
    #[must_use]
    pub fn bytes_per_pixel(&self) -> f32 {
        match self {
            Self::Rgb | Self::Bgr => 3.0,
            Self::Rgba | Self::Bgra | Self::Argb | Self::Abgr => 4.0,
            Self::Nv12 | Self::I420 => 1.5, // YUV 4:2:0
        }
    }

    /// Returns true if this is a planar format (Y, U, V in separate planes).
    #[must_use]
    pub fn is_planar(&self) -> bool {
        matches!(self, Self::Nv12 | Self::I420)
    }
}

impl Default for PixelFormat {
    fn default() -> Self {
        Self::Rgb
    }
}

/// Rate control mode for encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RateControlMode {
    /// Constant QP - fixed quality, variable bitrate.
    ConstQp,
    /// Variable Bitrate - targets average bitrate.
    Vbr,
    /// Constant Bitrate - strict bitrate control.
    Cbr,
}

impl Default for RateControlMode {
    fn default() -> Self {
        Self::Vbr
    }
}

/// Encoder preset controlling speed/quality tradeoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EncoderPreset {
    /// Fastest encoding, lowest quality (P1).
    Fastest,
    /// Fast encoding (P2).
    Fast,
    /// Medium speed (P4).
    Medium,
    /// Slow encoding, higher quality (P5).
    Slow,
    /// Slowest encoding, highest quality (P7).
    Slowest,
}

impl Default for EncoderPreset {
    fn default() -> Self {
        Self::Medium
    }
}

/// Tuning mode for the encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TuningMode {
    /// High quality encoding.
    HighQuality,
    /// Low latency encoding (good for real-time streaming).
    LowLatency,
    /// Ultra low latency encoding.
    UltraLowLatency,
    /// Lossless encoding.
    Lossless,
}

impl Default for TuningMode {
    fn default() -> Self {
        Self::LowLatency
    }
}

/// Configuration for creating a video encoder.
#[derive(Debug, Clone)]
pub struct EncoderConfig {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Video codec to use.
    pub codec: Codec,
    /// Framerate as (numerator, denominator).
    pub framerate: (u32, u32),
    /// Target bitrate in bits per second (for VBR/CBR).
    pub bitrate: u32,
    /// Maximum bitrate in bits per second (for VBR).
    pub max_bitrate: u32,
    /// Rate control mode.
    pub rate_control: RateControlMode,
    /// Encoder preset (speed/quality tradeoff).
    pub preset: EncoderPreset,
    /// Tuning mode.
    pub tuning: TuningMode,
    /// GOP (Group of Pictures) length. 0 for infinite.
    pub gop_length: u32,
    /// Number of B-frames between I and P frames.
    pub b_frames: u32,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            codec: Codec::H264,
            framerate: (30, 1),
            bitrate: 5_000_000,    // 5 Mbps
            max_bitrate: 8_000_000, // 8 Mbps
            rate_control: RateControlMode::Vbr,
            preset: EncoderPreset::Medium,
            tuning: TuningMode::LowLatency,
            gop_length: 30,        // Keyframe every 30 frames
            b_frames: 0,           // No B-frames for low latency
        }
    }
}

impl EncoderConfig {
    /// Create a new encoder configuration with the given dimensions.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            ..Default::default()
        }
    }

    /// Set the codec.
    #[must_use]
    pub fn codec(mut self, codec: Codec) -> Self {
        self.codec = codec;
        self
    }

    /// Set the framerate.
    #[must_use]
    pub fn framerate(mut self, num: u32, den: u32) -> Self {
        self.framerate = (num, den);
        self
    }

    /// Set the target bitrate in bits per second.
    #[must_use]
    pub fn bitrate(mut self, bitrate: u32) -> Self {
        self.bitrate = bitrate;
        self
    }

    /// Set the encoder preset.
    #[must_use]
    pub fn preset(mut self, preset: EncoderPreset) -> Self {
        self.preset = preset;
        self
    }

    /// Set the tuning mode.
    #[must_use]
    pub fn tuning(mut self, tuning: TuningMode) -> Self {
        self.tuning = tuning;
        self
    }

    /// Configure for low latency streaming.
    #[must_use]
    pub fn for_low_latency(mut self) -> Self {
        self.tuning = TuningMode::LowLatency;
        self.b_frames = 0;
        self.gop_length = 30;
        self
    }
}

/// Configuration for creating a video decoder.
#[derive(Debug, Clone)]
pub struct DecoderConfig {
    /// Video codec to decode.
    pub codec: Codec,
    /// Output pixel format (default: NV12).
    pub output_format: PixelFormat,
    /// Maximum decode surfaces (0 for auto).
    pub max_decode_surfaces: u32,
    /// Maximum display delay in frames (0 for low latency).
    pub max_display_delay: u32,
    /// Enable deinterlacing.
    pub deinterlace: bool,
    /// Target output width (0 = same as input).
    pub target_width: u32,
    /// Target output height (0 = same as input).
    pub target_height: u32,
}

impl Default for DecoderConfig {
    fn default() -> Self {
        Self {
            codec: Codec::H264,
            output_format: PixelFormat::Nv12,
            max_decode_surfaces: 0,  // Auto
            max_display_delay: 0,    // Low latency
            deinterlace: false,
            target_width: 0,         // Same as input
            target_height: 0,        // Same as input
        }
    }
}

impl DecoderConfig {
    /// Create a new decoder configuration for the given codec.
    #[must_use]
    pub fn new(codec: Codec) -> Self {
        Self {
            codec,
            ..Default::default()
        }
    }

    /// Set the output pixel format.
    #[must_use]
    pub fn output_format(mut self, format: PixelFormat) -> Self {
        self.output_format = format;
        self
    }

    /// Set the maximum display delay.
    #[must_use]
    pub fn max_display_delay(mut self, delay: u32) -> Self {
        self.max_display_delay = delay;
        self
    }

    /// Enable deinterlacing.
    #[must_use]
    pub fn deinterlace(mut self, enable: bool) -> Self {
        self.deinterlace = enable;
        self
    }

    /// Set target output dimensions (for scaling).
    #[must_use]
    pub fn target_size(mut self, width: u32, height: u32) -> Self {
        self.target_width = width;
        self.target_height = height;
        self
    }

    /// Configure for low latency decoding.
    #[must_use]
    pub fn for_low_latency(mut self) -> Self {
        self.max_display_delay = 0;
        self.max_decode_surfaces = 4;  // Minimum for low latency
        self
    }
}

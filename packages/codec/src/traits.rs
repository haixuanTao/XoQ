//! Core traits for video encoding/decoding.

use crate::{CodecError, Codec, PixelFormat};

/// Trait for types that can provide video frame data.
///
/// This trait allows different frame types to be used with the encoder
/// without requiring a specific concrete type.
pub trait VideoFrameData: Send {
    /// Returns the frame width in pixels.
    fn width(&self) -> u32;

    /// Returns the frame height in pixels.
    fn height(&self) -> u32;

    /// Returns the pixel format of the frame.
    fn pixel_format(&self) -> PixelFormat;

    /// Returns the raw frame data as a byte slice.
    fn data(&self) -> &[u8];

    /// Returns the frame timestamp in microseconds.
    fn timestamp_us(&self) -> u64;
}

/// Parameters for encoding a single frame.
#[derive(Debug, Clone, Default)]
pub struct EncodeParams {
    /// Force this frame to be a keyframe (IDR frame).
    pub force_keyframe: bool,
    /// Optional timestamp to use (overrides frame timestamp).
    pub timestamp_us: Option<u64>,
}

/// Result of encoding a frame.
#[derive(Debug, Clone)]
pub struct EncodedPacket {
    /// Encoded bitstream data (e.g., H.264 NAL units).
    pub data: Vec<u8>,
    /// Presentation timestamp in microseconds.
    pub pts_us: u64,
    /// Whether this packet contains a keyframe.
    pub is_keyframe: bool,
    /// Frame index (monotonically increasing).
    pub frame_index: u64,
}

impl EncodedPacket {
    /// Create a new encoded packet.
    #[must_use]
    pub fn new(data: Vec<u8>, pts_us: u64, is_keyframe: bool, frame_index: u64) -> Self {
        Self {
            data,
            pts_us,
            is_keyframe,
            frame_index,
        }
    }
}

/// Trait for video encoders.
///
/// Implementors of this trait can encode video frames into a compressed
/// bitstream format (e.g., H.264, HEVC, AV1).
///
/// # Example
///
/// ```ignore
/// use xoq_codec::{VideoEncoder, VideoFrame, EncoderConfig};
///
/// let config = EncoderConfig::new(1920, 1080)
///     .codec(Codec::H264)
///     .for_low_latency();
///
/// let mut encoder = NvencEncoder::new(config)?;
///
/// // Encode a frame
/// let frame = VideoFrame::from_rgb(1920, 1080, rgb_data, timestamp);
/// let packet = encoder.encode(&frame)?;
///
/// // packet.data contains H.264 NAL units
/// ```
pub trait VideoEncoder: Send + Sync {
    /// Encode a video frame.
    ///
    /// Returns an encoded packet on success. May return `CodecError::NeedMoreInput`
    /// if the encoder is buffering frames (e.g., for B-frame reordering).
    fn encode(&mut self, frame: &dyn VideoFrameData) -> Result<EncodedPacket, CodecError>;

    /// Encode a video frame with additional parameters.
    fn encode_with_params(
        &mut self,
        frame: &dyn VideoFrameData,
        params: EncodeParams,
    ) -> Result<EncodedPacket, CodecError>;

    /// Flush the encoder and return any remaining packets.
    ///
    /// This should be called when no more frames will be submitted
    /// to ensure all encoded data is retrieved.
    fn flush(&mut self) -> Result<Vec<EncodedPacket>, CodecError>;

    /// Returns the codec being used.
    fn codec(&self) -> Codec;

    /// Returns the configured dimensions (width, height).
    fn dimensions(&self) -> (u32, u32);
}

/// Result of decoding a packet.
///
/// Contains decoded frame data in the configured output pixel format.
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    /// Decoded pixel data in the output format.
    pub data: Vec<u8>,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Pixel format of the decoded frame.
    pub pixel_format: PixelFormat,
    /// Presentation timestamp in microseconds.
    pub pts_us: u64,
    /// Frame index (monotonically increasing).
    pub frame_index: u64,
}

impl DecodedFrame {
    /// Create a new decoded frame.
    #[must_use]
    pub fn new(
        data: Vec<u8>,
        width: u32,
        height: u32,
        pixel_format: PixelFormat,
        pts_us: u64,
        frame_index: u64,
    ) -> Self {
        Self {
            data,
            width,
            height,
            pixel_format,
            pts_us,
            frame_index,
        }
    }
}

/// Trait for video decoders.
///
/// Implementors of this trait can decode compressed video bitstreams
/// (e.g., H.264, HEVC, AV1) into raw video frames.
///
/// # Example
///
/// ```ignore
/// use xoq_codec::{VideoDecoder, DecoderConfig, Codec};
///
/// let config = DecoderConfig::new(Codec::H264)
///     .output_format(PixelFormat::Nv12);
///
/// let mut decoder = NvdecDecoder::new(config)?;
///
/// // Decode a packet
/// let packet = EncodedPacket { ... };
/// if let Some(frame) = decoder.decode(&packet)? {
///     // frame.data contains decoded pixels
/// }
///
/// // Flush remaining frames
/// for frame in decoder.flush()? {
///     // Process remaining frames
/// }
/// ```
pub trait VideoDecoder: Send {
    /// Decode an encoded packet.
    ///
    /// Returns `Ok(Some(frame))` when a decoded frame is available,
    /// `Ok(None)` when more input is needed, or an error on failure.
    ///
    /// Due to B-frame reordering or other codec requirements, there may be
    /// a delay between submitting a packet and receiving a decoded frame.
    fn decode(&mut self, packet: &EncodedPacket) -> Result<Option<DecodedFrame>, CodecError>;

    /// Flush the decoder and return any remaining frames.
    ///
    /// This should be called when no more packets will be submitted
    /// to ensure all decoded frames are retrieved.
    fn flush(&mut self) -> Result<Vec<DecodedFrame>, CodecError>;

    /// Returns the codec being used.
    fn codec(&self) -> Codec;

    /// Returns the current dimensions, if known.
    ///
    /// Dimensions may not be known until the first frame is decoded.
    fn dimensions(&self) -> Option<(u32, u32)>;

    /// Returns the output pixel format.
    fn output_format(&self) -> PixelFormat;
}

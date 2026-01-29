//! NVIDIA CUVID video parser bindings (nvcuvid.h)
//!
//! Raw FFI bindings to NVIDIA's CUVID video parser API for parsing
//! video bitstreams (H.264, HEVC, AV1, etc.) before decoding.

use std::os::raw::{c_char, c_int, c_uchar, c_uint, c_ulong, c_ulonglong, c_void};

use super::cuviddec::{cudaVideoChromaFormat, cudaVideoCodec, CUresult, CUVIDPICPARAMS};

/// Video parser handle
pub type CUvideoparser = *mut c_void;

// ============================================================================
// Parser callback types
// ============================================================================

/// Callback for sequence header (called when stream parameters change)
pub type PFNVIDSEQUENCECALLBACK = Option<
    unsafe extern "C" fn(pvUserData: *mut c_void, pVideoFormat: *mut CUVIDEOFORMAT) -> c_int,
>;

/// Callback for decoded picture (called when a picture needs decoding)
pub type PFNVIDDECODECALLBACK = Option<
    unsafe extern "C" fn(pvUserData: *mut c_void, pPicParams: *mut CUVIDPICPARAMS) -> c_int,
>;

/// Callback for display picture (called when a picture is ready for display)
pub type PFNVIDDISPLAYCALLBACK = Option<
    unsafe extern "C" fn(pvUserData: *mut c_void, pDispInfo: *mut CUVIDPARSERDISPINFO) -> c_int,
>;

/// Callback for getting operation point (for scalable streams)
pub type PFNVIDOPPOINTCALLBACK = Option<
    unsafe extern "C" fn(pvUserData: *mut c_void, pOPInfo: *mut CUVIDOPERATINGPOINTINFO) -> c_int,
>;

/// Callback for SEI messages
pub type PFNVIDSEIMSGCALLBACK = Option<
    unsafe extern "C" fn(pvUserData: *mut c_void, pSeiMessage: *mut CUVIDSEIMESSAGEINFO) -> c_int,
>;

// ============================================================================
// Video format structure
// ============================================================================

/// Video format (returned in sequence callback)
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct CUVIDEOFORMAT {
    /// Codec type
    pub codec: cudaVideoCodec,
    /// Frame rate (if field_rate is set, this is doubled)
    pub frame_rate: _CUVIDEOFORMAT__frame_rate,
    /// Progressive sequence
    pub progressive_sequence: c_uchar,
    /// Bit depth luma minus 8
    pub bit_depth_luma_minus8: c_uchar,
    /// Bit depth chroma minus 8
    pub bit_depth_chroma_minus8: c_uchar,
    /// Min number of decode surfaces
    pub min_num_decode_surfaces: c_uchar,
    /// Coded width
    pub coded_width: c_uint,
    /// Coded height
    pub coded_height: c_uint,
    /// Display area
    pub display_area: _CUVIDEOFORMAT__display_area,
    /// Chroma format
    pub chroma_format: cudaVideoChromaFormat,
    /// Bitrate (if available)
    pub bitrate: c_uint,
    /// Display aspect ratio x
    pub display_aspect_ratio: _CUVIDEOFORMAT__display_aspect_ratio,
    /// Video signal description
    pub video_signal_description: _CUVIDEOFORMAT__video_signal_description,
    /// Sequence header length
    pub seqhdr_data_length: c_uint,
}

/// Frame rate structure
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _CUVIDEOFORMAT__frame_rate {
    /// Numerator (e.g., 30000 for 29.97fps)
    pub numerator: c_uint,
    /// Denominator (e.g., 1001 for 29.97fps)
    pub denominator: c_uint,
}

/// Display area structure
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _CUVIDEOFORMAT__display_area {
    pub left: c_int,
    pub top: c_int,
    pub right: c_int,
    pub bottom: c_int,
}

/// Display aspect ratio structure
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _CUVIDEOFORMAT__display_aspect_ratio {
    pub x: c_int,
    pub y: c_int,
}

/// Video signal description
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _CUVIDEOFORMAT__video_signal_description {
    /// Video format (component, PAL, NTSC, SECAM, MAC, unspecified)
    pub video_format: c_uchar,
    /// Full range flag
    pub video_full_range_flag: c_uchar,
    /// Reserved
    pub reserved_zero_bits: c_uchar,
    /// Colour description present flag
    pub color_primaries: c_uchar,
    /// Transfer characteristics
    pub transfer_characteristics: c_uchar,
    /// Matrix coefficients
    pub matrix_coefficients: c_uchar,
}

// ============================================================================
// Parser display info
// ============================================================================

/// Display information (returned in display callback)
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct CUVIDPARSERDISPINFO {
    /// Picture index
    pub picture_index: c_int,
    /// Progressive frame flag
    pub progressive_frame: c_int,
    /// Top field first flag
    pub top_field_first: c_int,
    /// Repeat first field flag
    pub repeat_first_field: c_int,
    /// Timestamp (presentation time)
    pub timestamp: c_ulonglong,
}

// ============================================================================
// Operating point info (for scalable streams like AV1)
// ============================================================================

/// Operating point information
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct CUVIDOPERATINGPOINTINFO {
    /// Codec type
    pub codec: cudaVideoCodec,
    /// Operating points mask
    pub av1: _CUVIDOPERATINGPOINTINFO__av1,
}

/// AV1 operating point info
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _CUVIDOPERATINGPOINTINFO__av1 {
    /// Number of operating points
    pub operating_points_cnt: c_uchar,
    /// Reserved
    pub reserved24_bits: [c_uchar; 3],
    /// Operating points IDC
    pub operating_points_idc: [c_uint; 32],
}

// ============================================================================
// SEI message info
// ============================================================================

/// SEI message information
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct CUVIDSEIMESSAGEINFO {
    /// SEI message payload
    pub pSEIData: *mut c_void,
    /// SEI message types
    pub pSEIMessage: *mut CUSEIMESSAGE,
    /// Number of messages
    pub sei_message_count: c_uint,
    /// Picture index
    pub picIdx: c_uint,
}

/// SEI message
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct CUSEIMESSAGE {
    /// SEI payload type
    pub sei_message_type: c_uchar,
    /// Reserved
    pub reserved: [c_uchar; 3],
    /// SEI payload size
    pub sei_message_size: c_uint,
}

// ============================================================================
// Source data packet
// ============================================================================

/// Flags for source data packet
pub const CUVID_PKT_ENDOFSTREAM: c_ulong = 0x01;
pub const CUVID_PKT_TIMESTAMP: c_ulong = 0x02;
pub const CUVID_PKT_DISCONTINUITY: c_ulong = 0x04;
pub const CUVID_PKT_ENDOFPICTURE: c_ulong = 0x08;
pub const CUVID_PKT_NOTIFY_EOS: c_ulong = 0x10;

/// Source data packet (input to parser)
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct CUVIDSOURCEDATAPACKET {
    /// Flags (combination of CUVID_PKT_*)
    pub flags: c_ulong,
    /// Payload size (number of bytes)
    pub payload_size: c_ulong,
    /// Pointer to payload data
    pub payload: *const c_uchar,
    /// Timestamp (presentation timestamp)
    pub timestamp: c_ulonglong,
}

impl Default for CUVIDSOURCEDATAPACKET {
    fn default() -> Self {
        Self {
            flags: 0,
            payload_size: 0,
            payload: std::ptr::null(),
            timestamp: 0,
        }
    }
}

// ============================================================================
// Parser creation parameters
// ============================================================================

/// Parser creation parameters
#[repr(C)]
#[derive(Debug, Clone)]
pub struct CUVIDPARSERPARAMS {
    /// Codec type
    pub CodecType: cudaVideoCodec,
    /// Max number of decode surfaces (0 = default)
    pub ulMaxNumDecodeSurfaces: c_uint,
    /// Clock rate (0 = default 10MHz)
    pub ulClockRate: c_uint,
    /// Error threshold (0-100, 0 = decode all)
    pub ulErrorThreshold: c_uint,
    /// Max display delay (0 = no delay)
    pub ulMaxDisplayDelay: c_uint,

    /// Reserved
    pub uReserved1: [c_uint; 5],

    /// User data pointer (passed to callbacks)
    pub pUserData: *mut c_void,
    /// Sequence change callback
    pub pfnSequenceCallback: PFNVIDSEQUENCECALLBACK,
    /// Decode callback
    pub pfnDecodePicture: PFNVIDDECODECALLBACK,
    /// Display callback
    pub pfnDisplayPicture: PFNVIDDISPLAYCALLBACK,

    /// Reserved
    pub pvReserved2: *mut c_void,
    /// Operating point callback (for AV1)
    pub pfnGetOperatingPoint: PFNVIDOPPOINTCALLBACK,
    /// SEI message callback
    pub pfnGetSEIMsg: PFNVIDSEIMSGCALLBACK,

    /// Reserved
    pub pvReserved3: [*mut c_void; 5],

    /// Sequence header data (optional)
    pub pExtVideoInfo: *mut CUVIDEOFORMATEX,
}

impl Default for CUVIDPARSERPARAMS {
    fn default() -> Self {
        Self {
            CodecType: cudaVideoCodec::cudaVideoCodec_H264,
            ulMaxNumDecodeSurfaces: 0,
            ulClockRate: 0,
            ulErrorThreshold: 0,
            ulMaxDisplayDelay: 0,
            uReserved1: [0; 5],
            pUserData: std::ptr::null_mut(),
            pfnSequenceCallback: None,
            pfnDecodePicture: None,
            pfnDisplayPicture: None,
            pvReserved2: std::ptr::null_mut(),
            pfnGetOperatingPoint: None,
            pfnGetSEIMsg: None,
            pvReserved3: [std::ptr::null_mut(); 5],
            pExtVideoInfo: std::ptr::null_mut(),
        }
    }
}

/// Extended video format info (with raw sequence header)
#[repr(C)]
#[derive(Clone)]
pub struct CUVIDEOFORMATEX {
    /// Video format
    pub format: CUVIDEOFORMAT,
    /// Raw sequence header data
    pub raw_seqhdr_data: [c_uchar; 1024],
}

impl Default for CUVIDEOFORMATEX {
    fn default() -> Self {
        Self {
            format: unsafe { std::mem::zeroed() },
            raw_seqhdr_data: [0; 1024],
        }
    }
}

impl Default for CUVIDEOFORMAT {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

// ============================================================================
// External function declarations
// ============================================================================

extern "C" {
    /// Create a video parser
    pub fn cuvidCreateVideoParser(
        pObj: *mut CUvideoparser,
        pParams: *mut CUVIDPARSERPARAMS,
    ) -> CUresult;

    /// Parse a video data packet
    pub fn cuvidParseVideoData(
        obj: CUvideoparser,
        pPacket: *mut CUVIDSOURCEDATAPACKET,
    ) -> CUresult;

    /// Destroy a video parser
    pub fn cuvidDestroyVideoParser(obj: CUvideoparser) -> CUresult;
}

// ============================================================================
// Source API (for file/stream-based decoding)
// ============================================================================

/// Video source handle
pub type CUvideosource = *mut c_void;

/// Video source state enumeration
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum cudaVideoSourceState {
    cudaVideoState_Error = -1,
    cudaVideoState_Stopped = 0,
    cudaVideoState_Started = 1,
}

/// Data packet for source callback
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct CUVIDSOURCEPARAMS {
    /// Input class type
    pub ulClockRate: c_uint,
    /// Reserved
    pub uReserved1: [c_uint; 7],
    /// User data
    pub pUserData: *mut c_void,
    /// Audio callback
    pub pfnVideoDataHandler: PFNVIDSOURCECALLBACK,
    /// Video callback
    pub pfnAudioDataHandler: PFNVIDSOURCECALLBACK,
    /// Reserved
    pub pvReserved2: [*mut c_void; 8],
}

/// Source data callback
pub type PFNVIDSOURCECALLBACK = Option<
    unsafe extern "C" fn(pUserData: *mut c_void, pPacket: *mut CUVIDSOURCEDATAPACKET) -> c_int,
>;

extern "C" {
    /// Create a video source
    pub fn cuvidCreateVideoSource(
        pObj: *mut CUvideosource,
        pszFileName: *const c_char,
        pParams: *mut CUVIDSOURCEPARAMS,
    ) -> CUresult;

    /// Create a video source (wide char)
    pub fn cuvidCreateVideoSourceW(
        pObj: *mut CUvideosource,
        pwszFileName: *const u16,
        pParams: *mut CUVIDSOURCEPARAMS,
    ) -> CUresult;

    /// Destroy a video source
    pub fn cuvidDestroyVideoSource(obj: CUvideosource) -> CUresult;

    /// Set video source state
    pub fn cuvidSetVideoSourceState(
        obj: CUvideosource,
        state: cudaVideoSourceState,
    ) -> CUresult;

    /// Get video source state
    pub fn cuvidGetVideoSourceState(obj: CUvideosource) -> cudaVideoSourceState;

    /// Get source video format
    pub fn cuvidGetSourceVideoFormat(
        obj: CUvideosource,
        pvidfmt: *mut CUVIDEOFORMAT,
        flags: c_uint,
    ) -> CUresult;

    /// Get source audio format
    pub fn cuvidGetSourceAudioFormat(
        obj: CUvideosource,
        paudfmt: *mut CUAUDIOFORMAT,
        flags: c_uint,
    ) -> CUresult;
}

/// Audio format
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct CUAUDIOFORMAT {
    /// Codec type
    pub codec: cudaAudioCodec,
    /// Number of channels
    pub channels: c_uint,
    /// Sample rate
    pub samplespersec: c_uint,
    /// Bits per sample
    pub bitrate: c_uint,
    /// Reserved
    pub reserved1: c_uint,
    /// Reserved
    pub reserved2: [c_uint; 4],
}

/// Audio codec enumeration
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum cudaAudioCodec {
    cudaAudioCodec_MPEG1 = 0,
    cudaAudioCodec_MPEG2 = 1,
    cudaAudioCodec_MP3 = 2,
    cudaAudioCodec_AC3 = 3,
    cudaAudioCodec_LPCM = 4,
    cudaAudioCodec_AAC = 5,
}

// ============================================================================
// Helper types
// ============================================================================

/// Helper to convert codec enum to CUVID codec type
pub fn codec_to_cuvid(codec: &str) -> cudaVideoCodec {
    match codec {
        "h264" | "H264" | "avc" | "AVC" => cudaVideoCodec::cudaVideoCodec_H264,
        "h265" | "H265" | "hevc" | "HEVC" => cudaVideoCodec::cudaVideoCodec_HEVC,
        "av1" | "AV1" => cudaVideoCodec::cudaVideoCodec_AV1,
        "vp8" | "VP8" => cudaVideoCodec::cudaVideoCodec_VP8,
        "vp9" | "VP9" => cudaVideoCodec::cudaVideoCodec_VP9,
        "mpeg1" | "MPEG1" => cudaVideoCodec::cudaVideoCodec_MPEG1,
        "mpeg2" | "MPEG2" => cudaVideoCodec::cudaVideoCodec_MPEG2,
        "mpeg4" | "MPEG4" => cudaVideoCodec::cudaVideoCodec_MPEG4,
        "jpeg" | "JPEG" => cudaVideoCodec::cudaVideoCodec_JPEG,
        "vc1" | "VC1" => cudaVideoCodec::cudaVideoCodec_VC1,
        _ => cudaVideoCodec::cudaVideoCodec_H264,
    }
}

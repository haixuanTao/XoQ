//! NVIDIA CUVID decoder bindings (cuviddec.h)
//!
//! Raw FFI bindings to NVIDIA's CUVID decoder API for hardware-accelerated
//! video decoding using NVDEC.

use std::os::raw::{c_int, c_uint, c_ulong, c_ulonglong, c_void};

/// CUDA video decoder handle
pub type CUvideodecoder = *mut c_void;

/// CUDA video context lock (for multi-threaded access)
pub type CUvideoctxlock = *mut c_void;

/// Result type for CUVID decoder operations
pub type CUresult = c_int;

// ============================================================================
// Result codes
// ============================================================================

pub const CUDA_SUCCESS: CUresult = 0;
pub const CUDA_ERROR_INVALID_VALUE: CUresult = 1;
pub const CUDA_ERROR_OUT_OF_MEMORY: CUresult = 2;
pub const CUDA_ERROR_NOT_INITIALIZED: CUresult = 3;
pub const CUDA_ERROR_DEINITIALIZED: CUresult = 4;
pub const CUDA_ERROR_NO_DEVICE: CUresult = 100;
pub const CUDA_ERROR_INVALID_DEVICE: CUresult = 101;
pub const CUDA_ERROR_INVALID_CONTEXT: CUresult = 201;

// ============================================================================
// Video codec types
// ============================================================================

/// Video codec type enumeration
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum cudaVideoCodec {
    cudaVideoCodec_MPEG1 = 0,
    cudaVideoCodec_MPEG2 = 1,
    cudaVideoCodec_MPEG4 = 2,
    cudaVideoCodec_VC1 = 3,
    cudaVideoCodec_H264 = 4,
    cudaVideoCodec_JPEG = 5,
    cudaVideoCodec_H264_SVC = 6,
    cudaVideoCodec_H264_MVC = 7,
    cudaVideoCodec_HEVC = 8,
    cudaVideoCodec_VP8 = 9,
    cudaVideoCodec_VP9 = 10,
    cudaVideoCodec_AV1 = 11,
    cudaVideoCodec_NumCodecs = 12,
}

/// Surface format enumeration
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum cudaVideoSurfaceFormat {
    cudaVideoSurfaceFormat_NV12 = 0,
    cudaVideoSurfaceFormat_P016 = 1,
    cudaVideoSurfaceFormat_YUV444 = 2,
    cudaVideoSurfaceFormat_YUV444_16Bit = 3,
}

/// Chroma format enumeration
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum cudaVideoChromaFormat {
    cudaVideoChromaFormat_Monochrome = 0,
    cudaVideoChromaFormat_420 = 1,
    cudaVideoChromaFormat_422 = 2,
    cudaVideoChromaFormat_444 = 3,
}

/// Deinterlace mode enumeration
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum cudaVideoDeinterlaceMode {
    cudaVideoDeinterlaceMode_Weave = 0,
    cudaVideoDeinterlaceMode_Bob = 1,
    cudaVideoDeinterlaceMode_Adaptive = 2,
}

/// Video decoder creation flags
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum cudaVideoCreateFlags {
    cudaVideoCreate_Default = 0,
    cudaVideoCreate_PreferCUDA = 1,
    cudaVideoCreate_PreferDXVA = 2,
    cudaVideoCreate_PreferCUVID = 4,
}

// ============================================================================
// Decode capability structures
// ============================================================================

/// Decoder capability query structure (input)
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct CUVIDDECODECAPS {
    /// Codec type (IN)
    pub eCodecType: cudaVideoCodec,
    /// Chroma format (IN)
    pub eChromaFormat: cudaVideoChromaFormat,
    /// Bit depth of luma samples (IN)
    pub nBitDepthMinus8: c_uint,
    /// Reserved for future use (IN)
    pub reserved1: [c_uint; 3],

    /// 1 if codec/chroma supported, 0 otherwise (OUT)
    pub bIsSupported: c_uint,
    /// Number of NVDECs that can support IN params (OUT)
    pub nNumNVDECs: c_uint,
    /// Max supported coded width (OUT)
    pub nMaxWidth: c_uint,
    /// Max supported coded height (OUT)
    pub nMaxHeight: c_uint,
    /// Max macroblocks per second (OUT)
    pub nMaxMBCount: c_uint,
    /// Min supported coded width (OUT)
    pub nMinWidth: c_uint,
    /// Min supported coded height (OUT)
    pub nMinHeight: c_uint,
    /// 1 if histogram output is supported for given IN params (OUT)
    pub bIsHistogramSupported: c_uint,
    /// Histogram counter bit depth (OUT)
    pub nCounterBitDepth: c_uint,
    /// Max histogram bins (OUT)
    pub nMaxHistogramBins: c_uint,
    /// Reserved for future use
    pub reserved3: [c_uint; 10],
}

// ============================================================================
// Decoder creation structures
// ============================================================================

/// Structure used in creating the decoder
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct CUVIDDECODECREATEINFO {
    /// Coded width
    pub ulWidth: c_ulong,
    /// Coded height
    pub ulHeight: c_ulong,
    /// Max decode surfaces (parser's max_num_decode_surfaces)
    pub ulNumDecodeSurfaces: c_ulong,
    /// Codec type
    pub CodecType: cudaVideoCodec,
    /// Chroma format
    pub ChromaFormat: cudaVideoChromaFormat,
    /// Reserved for internal use
    pub ulCreationFlags: c_ulong,
    /// Bit depth of luma component
    pub bitDepthMinus8: c_ulong,
    /// Internal decoded buffer format
    pub ulIntraDecodeOnly: c_ulong,
    /// Reserved (must be 0)
    pub ulMaxWidth: c_ulong,
    /// Reserved (must be 0)
    pub ulMaxHeight: c_ulong,
    /// Reserved for future use
    pub Reserved1: c_ulong,

    /// Display area
    pub display_area: _CUVIDDECODECREATEINFO__display_area,

    /// Output format
    pub OutputFormat: cudaVideoSurfaceFormat,
    /// Deinterlace mode
    pub DeinterlaceMode: cudaVideoDeinterlaceMode,
    /// Target width (scaled output)
    pub ulTargetWidth: c_ulong,
    /// Target height (scaled output)
    pub ulTargetHeight: c_ulong,
    /// Number of output surfaces (0 = default)
    pub ulNumOutputSurfaces: c_ulong,
    /// Video context lock
    pub vidLock: CUvideoctxlock,

    /// Target rectangle
    pub target_rect: _CUVIDDECODECREATEINFO__target_rect,

    /// Enable histogram output
    pub enableHistogram: c_ulong,
    /// Reserved for future use
    pub Reserved2: [c_ulong; 4],
}

/// Display area within CUVIDDECODECREATEINFO
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _CUVIDDECODECREATEINFO__display_area {
    pub left: c_int,
    pub top: c_int,
    pub right: c_int,
    pub bottom: c_int,
}

/// Target rectangle within CUVIDDECODECREATEINFO
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _CUVIDDECODECREATEINFO__target_rect {
    pub left: c_int,
    pub top: c_int,
    pub right: c_int,
    pub bottom: c_int,
}

// ============================================================================
// Picture parameters for decoding
// ============================================================================

/// Picture parameters structure
#[repr(C)]
#[derive(Copy, Clone)]
pub struct CUVIDPICPARAMS {
    /// Picture index of current picture
    pub PicWidthInMbs: c_int,
    /// Picture width in macroblocks
    pub FrameHeightInMbs: c_int,
    /// Frame height in macroblocks
    pub CurrPicIdx: c_int,
    /// Picture index of the decoded picture
    pub field_pic_flag: c_int,
    /// 1 if field picture, 0 if frame
    pub bottom_field_flag: c_int,
    /// 1 if bottom field, 0 if top field
    pub second_field: c_int,
    /// 1 if second field of a complementary field pair
    /// Bitstream data
    pub nBitstreamDataLen: c_uint,
    /// Number of bytes in bitstream data
    pub pBitstreamData: *const c_void,
    /// Pointer to bitstream data
    pub nNumSlices: c_uint,
    /// Number of slices
    pub pSliceDataOffsets: *const c_uint,
    /// Slice data offsets
    pub ref_pic_flag: c_int,
    /// This picture is a reference picture
    pub intra_pic_flag: c_int,
    /// This picture is entirely intra coded

    /// Reserved for alignment
    pub Reserved: [c_uint; 30],

    /// Codec-specific data
    pub CodecSpecific: CUVIDPICPARAMS_CodecSpecific,
}

/// Union for codec-specific picture parameters
#[repr(C)]
#[derive(Copy, Clone)]
pub union CUVIDPICPARAMS_CodecSpecific {
    pub h264: CUVIDH264PICPARAMS,
    pub hevc: CUVIDHEVCPICPARAMS,
    pub av1: CUVIDAV1PICPARAMS,
    pub CodecReserved: [c_uint; 1024],
}

/// H.264 picture parameters
#[repr(C)]
#[derive(Copy, Clone)]
pub struct CUVIDH264PICPARAMS {
    /// Log2 of max frame num minus 4
    pub log2_max_frame_num_minus4: c_int,
    /// Picture order count type
    pub pic_order_cnt_type: c_int,
    /// Log2 of max pic order count minus 4
    pub log2_max_pic_order_cnt_lsb_minus4: c_int,
    /// Delta pic order always zero flag
    pub delta_pic_order_always_zero_flag: c_int,
    /// Frame MBS only flag
    pub frame_mbs_only_flag: c_int,
    /// Direct 8x8 inference flag
    pub direct_8x8_inference_flag: c_int,
    /// Number of reference frames
    pub num_ref_frames: c_int,
    /// Residual colour transform flag
    pub residual_colour_transform_flag: c_int,
    /// Bit depth luma minus 8
    pub bit_depth_luma_minus8: c_int,
    /// Bit depth chroma minus 8
    pub bit_depth_chroma_minus8: c_int,
    /// QP BD offset y
    pub qpprime_y_zero_transform_bypass_flag: c_int,

    /// Entropy coding mode flag
    pub entropy_coding_mode_flag: c_int,
    /// Pic order present flag
    pub pic_order_present_flag: c_int,
    /// Num ref idx L0 default active minus 1
    pub num_ref_idx_l0_default_active_minus1: c_int,
    /// Num ref idx L1 default active minus 1
    pub num_ref_idx_l1_default_active_minus1: c_int,
    /// Weighted pred flag
    pub weighted_pred_flag: c_int,
    /// Weighted bipred idc
    pub weighted_bipred_idc: c_int,
    /// Pic init QP minus 26
    pub pic_init_qp_minus26: c_int,
    /// Deblocking filter control present flag
    pub deblocking_filter_control_present_flag: c_int,
    /// Redundant pic cnt present flag
    pub redundant_pic_cnt_present_flag: c_int,
    /// Transform 8x8 mode flag
    pub transform_8x8_mode_flag: c_int,
    /// MbaffFrameFlag
    pub MbsffFrameFlag: c_int,
    /// Constrained intra pred flag
    pub constrained_intra_pred_flag: c_int,
    /// Chroma QP index offset
    pub chroma_qp_index_offset: c_int,
    /// Second chroma QP index offset
    pub second_chroma_qp_index_offset: c_int,
    /// Frame number
    pub frame_num: c_int,
    /// Chroma format idc
    pub CurrFieldOrderCnt: [c_int; 2],
    /// Reference frames
    pub dpb: [CUVIDH264DPBENTRY; 16],

    /// Scaling lists present
    pub fmo_aso_enable: c_int,
    /// Number of slice groups minus 1
    pub num_slice_groups_minus1: c_int,
    /// Slice group map type
    pub slice_group_map_type: c_int,
    /// Pic size in map units minus 1
    pub pic_init_qs_minus26: c_int,
    /// Slice group change direction flag
    pub slice_group_change_direction_flag: c_int,
    /// Slice group change rate minus 1
    pub slice_group_change_rate_minus1: c_int,

    /// Reserved
    pub Reserved: [c_uint; 12],
    /// FMO/ASO data
    pub fmo: CUVIDH264FMOASO,
}

/// H.264 DPB entry
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct CUVIDH264DPBENTRY {
    /// Picture index
    pub PicIdx: c_int,
    /// Frame index
    pub FrameIdx: c_int,
    /// Is long term reference
    pub is_long_term: c_int,
    /// Non-existing (gap in frame_num)
    pub not_existing: c_int,
    /// Used for reference
    pub used_for_reference: c_int,
    /// Field order count
    pub FieldOrderCnt: [c_int; 2],
}

/// H.264 FMO/ASO structure
#[repr(C)]
#[derive(Copy, Clone)]
pub struct CUVIDH264FMOASO {
    /// Reserved
    pub Reserved: [c_uint; 64],
}

/// HEVC picture parameters
#[repr(C)]
#[derive(Copy, Clone)]
pub struct CUVIDHEVCPICPARAMS {
    /// Picture width in luma samples
    pub pic_width_in_luma_samples: c_int,
    /// Picture height in luma samples
    pub pic_height_in_luma_samples: c_int,
    /// Log2 min luma coding block size minus 3
    pub log2_min_luma_coding_block_size_minus3: c_int,
    /// Log2 diff max min luma coding block size
    pub log2_diff_max_min_luma_coding_block_size: c_int,
    /// Log2 min transform block size minus 2
    pub log2_min_transform_block_size_minus2: c_int,
    /// Log2 diff max min transform block size
    pub log2_diff_max_min_transform_block_size: c_int,
    /// PCM enabled flag
    pub pcm_enabled_flag: c_int,
    /// Log2 min PCM luma coding block size minus 3
    pub log2_min_pcm_luma_coding_block_size_minus3: c_int,
    /// Log2 diff max min PCM luma coding block size
    pub log2_diff_max_min_pcm_luma_coding_block_size: c_int,
    /// PCM sample bit depth luma minus 1
    pub pcm_sample_bit_depth_luma_minus1: c_int,
    /// PCM sample bit depth chroma minus 1
    pub pcm_sample_bit_depth_chroma_minus1: c_int,
    /// PCM loop filter disabled flag
    pub pcm_loop_filter_disabled_flag: c_int,
    /// Strong intra smoothing enabled flag
    pub strong_intra_smoothing_enabled_flag: c_int,
    /// Max transform hierarchy depth inter
    pub max_transform_hierarchy_depth_inter: c_int,
    /// Max transform hierarchy depth intra
    pub max_transform_hierarchy_depth_intra: c_int,
    /// Amp enabled flag
    pub amp_enabled_flag: c_int,
    /// Separate colour plane flag
    pub separate_colour_plane_flag: c_int,
    /// Log2 max pic order cnt lsb minus 4
    pub log2_max_pic_order_cnt_lsb_minus4: c_int,
    /// Num short term ref pic sets
    pub num_short_term_ref_pic_sets: c_int,
    /// Long term ref pics present flag
    pub long_term_ref_pics_present_flag: c_int,
    /// Num long term ref pics SPS
    pub num_long_term_ref_pics_sps: c_int,
    /// SPS temporal MVPD enabled flag
    pub sps_temporal_mvp_enabled_flag: c_int,
    /// Sample adaptive offset enabled flag
    pub sample_adaptive_offset_enabled_flag: c_int,
    /// Scaling list enabled flag
    pub scaling_list_enable_flag: c_int,

    /// Reserved
    pub Reserved0: [c_uint; 8],

    /// IRAPs present
    pub IrapPicFlag: c_int,
    /// IDR pic flag
    pub IdrPicFlag: c_int,
    /// Bit depth luma minus 8
    pub bit_depth_luma_minus8: c_int,
    /// Bit depth chroma minus 8
    pub bit_depth_chroma_minus8: c_int,

    /// Reserved
    pub Reserved1: [c_uint; 14],

    /// Dependent slice segments enabled flag
    pub dependent_slice_segments_enabled_flag: c_int,
    /// Slice segment header extension present flag
    pub slice_segment_header_extension_present_flag: c_int,
    /// Sign data hiding enabled flag
    pub sign_data_hiding_enabled_flag: c_int,
    /// Output flag present flag
    pub output_flag_present_flag: c_int,
    /// Num extra slice header bits
    pub num_extra_slice_header_bits: c_int,
    /// Tiles enabled flag
    pub tiles_enabled_flag: c_int,
    /// Entropy coding sync enabled flag
    pub entropy_coding_sync_enabled_flag: c_int,
    /// Num tile columns minus 1
    pub num_tile_columns_minus1: c_int,
    /// Num tile rows minus 1
    pub num_tile_rows_minus1: c_int,
    /// Uniform spacing flag
    pub uniform_spacing_flag: c_int,
    /// Loop filter across tiles enabled flag
    pub loop_filter_across_tiles_enabled_flag: c_int,
    /// Loop filter across slices enabled flag
    pub pps_loop_filter_across_slices_enabled_flag: c_int,
    /// Deblocking filter control present flag
    pub deblocking_filter_control_present_flag: c_int,
    /// Deblocking filter override enabled flag
    pub deblocking_filter_override_enabled_flag: c_int,
    /// PPS deblocking filter disabled flag
    pub pps_deblocking_filter_disabled_flag: c_int,
    /// Beta offset div 2
    pub pps_beta_offset_div2: c_int,
    /// TC offset div 2
    pub pps_tc_offset_div2: c_int,
    /// Lists modification present flag
    pub lists_modification_present_flag: c_int,
    /// Log2 parallel merge level minus 2
    pub log2_parallel_merge_level_minus2: c_int,
    /// Slice segment header extension present flag
    pub slice_segment_header_extension_present_flag2: c_int,

    /// Reserved
    pub Reserved2: [c_uint; 32],

    /// Num ref idx L0 default active minus 1
    pub num_ref_idx_l0_default_active_minus1: c_int,
    /// Num ref idx L1 default active minus 1
    pub num_ref_idx_l1_default_active_minus1: c_int,
    /// Init QP minus 26
    pub init_qp_minus26: c_int,
    /// Use DQP flag
    pub constrained_intra_pred_flag: c_int,
    /// CU QP delta enabled flag
    pub cu_qp_delta_enabled_flag: c_int,
    /// Diff CU QP delta depth
    pub diff_cu_qp_delta_depth: c_int,
    /// CB QP offset
    pub pps_cb_qp_offset: c_int,
    /// CR QP offset
    pub pps_cr_qp_offset: c_int,
    /// Slice chroma QP offsets present flag
    pub pps_slice_chroma_qp_offsets_present_flag: c_int,
    /// Weighted pred flag
    pub weighted_pred_flag: c_int,
    /// Weighted bipred flag
    pub weighted_bipred_flag: c_int,
    /// Transform skip enabled flag
    pub transform_skip_enabled_flag: c_int,
    /// Transquant bypass enabled flag
    pub transquant_bypass_enabled_flag: c_int,
    /// Cabac init present flag
    pub cabac_init_present_flag: c_int,

    /// Reserved
    pub Reserved3: [c_uint; 24],

    /// Reference frames
    pub RefPicIdx: [c_int; 16],
    /// POC
    pub PicOrderCntVal: c_int,
    /// Is reference
    pub IsLongTerm: [c_int; 16],
    /// RPL
    pub RefPicSetStCurrBefore: [c_int; 8],
    /// RPL
    pub RefPicSetStCurrAfter: [c_int; 8],
    /// RPL
    pub RefPicSetLtCurr: [c_int; 8],

    /// Reserved
    pub Reserved4: [c_uint; 256],
}

/// AV1 picture parameters (placeholder - full struct is very large)
#[repr(C)]
#[derive(Copy, Clone)]
pub struct CUVIDAV1PICPARAMS {
    /// Reserved
    pub Reserved: [c_uint; 1024],
}

// ============================================================================
// Decoded frame information
// ============================================================================

/// Parameters for cuvidGetDecodeStatus
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct CUVIDGETDECODESTATUS {
    /// Decode status
    pub decodeStatus: cuvidDecodeStatus,
    /// Reserved
    pub reserved: [c_uint; 255],
    /// Reserved
    pub pReserved: *mut c_void,
}

/// Decode status enumeration
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum cuvidDecodeStatus {
    cuvidDecodeStatus_Invalid = 0,
    cuvidDecodeStatus_InProgress = 1,
    cuvidDecodeStatus_Success = 2,
    cuvidDecodeStatus_Error = 8,
    cuvidDecodeStatus_Error_Concealed = 9,
}

/// Parameters for cuvidReconfigureDecoder
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct CUVIDRECONFIGUREDECODERINFO {
    /// New width
    pub ulWidth: c_ulong,
    /// New height
    pub ulHeight: c_ulong,
    /// Target width
    pub ulTargetWidth: c_ulong,
    /// Target height
    pub ulTargetHeight: c_ulong,
    /// Number of decode surfaces
    pub ulNumDecodeSurfaces: c_ulong,
    /// Reserved
    pub reserved1: [c_ulong; 12],
    /// Display area
    pub display_area: _CUVIDRECONFIGUREDECODERINFO__display_area,
    /// Target rect
    pub target_rect: _CUVIDRECONFIGUREDECODERINFO__target_rect,
    /// Reserved
    pub reserved2: [c_ulong; 11],
}

/// Display area for reconfigure
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _CUVIDRECONFIGUREDECODERINFO__display_area {
    pub left: c_int,
    pub top: c_int,
    pub right: c_int,
    pub bottom: c_int,
}

/// Target rect for reconfigure
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _CUVIDRECONFIGUREDECODERINFO__target_rect {
    pub left: c_int,
    pub top: c_int,
    pub right: c_int,
    pub bottom: c_int,
}

// ============================================================================
// External function declarations
// ============================================================================

extern "C" {
    /// Query decoder capabilities
    pub fn cuvidGetDecoderCaps(pdc: *mut CUVIDDECODECAPS) -> CUresult;

    /// Create a decoder
    pub fn cuvidCreateDecoder(
        phDecoder: *mut CUvideodecoder,
        pdci: *mut CUVIDDECODECREATEINFO,
    ) -> CUresult;

    /// Destroy a decoder
    pub fn cuvidDestroyDecoder(hDecoder: CUvideodecoder) -> CUresult;

    /// Decode a picture
    pub fn cuvidDecodePicture(
        hDecoder: CUvideodecoder,
        pPicParams: *mut CUVIDPICPARAMS,
    ) -> CUresult;

    /// Get decode status
    pub fn cuvidGetDecodeStatus(
        hDecoder: CUvideodecoder,
        nPicIdx: c_int,
        pDecodeStatus: *mut CUVIDGETDECODESTATUS,
    ) -> CUresult;

    /// Reconfigure the decoder (for adaptive streaming)
    pub fn cuvidReconfigureDecoder(
        hDecoder: CUvideodecoder,
        pDecReconfigParams: *mut CUVIDRECONFIGUREDECODERINFO,
    ) -> CUresult;

    /// Map a video frame for display
    pub fn cuvidMapVideoFrame64(
        hDecoder: CUvideodecoder,
        nPicIdx: c_int,
        pDevPtr: *mut c_ulonglong,
        pPitch: *mut c_uint,
        pVPP: *mut CUVIDPROCPARAMS,
    ) -> CUresult;

    /// Unmap a video frame
    pub fn cuvidUnmapVideoFrame64(hDecoder: CUvideodecoder, DevPtr: c_ulonglong) -> CUresult;

    /// Create a video context lock
    pub fn cuvidCtxLockCreate(pLock: *mut CUvideoctxlock, ctx: *mut c_void) -> CUresult;

    /// Destroy a video context lock
    pub fn cuvidCtxLockDestroy(lck: CUvideoctxlock) -> CUresult;

    /// Lock the video context
    pub fn cuvidCtxLock(lck: CUvideoctxlock, reserved_flags: c_uint) -> CUresult;

    /// Unlock the video context
    pub fn cuvidCtxUnlock(lck: CUvideoctxlock, reserved_flags: c_uint) -> CUresult;
}

/// Video processing parameters
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct CUVIDPROCPARAMS {
    /// Progressive frame flag
    pub progressive_frame: c_int,
    /// Second field flag
    pub second_field: c_int,
    /// Top field first flag
    pub top_field_first: c_int,
    /// Unpaired field flag
    pub unpaired_field: c_int,
    /// Reserved flags
    pub reserved_flags: c_uint,
    /// Reserved
    pub reserved_zero: c_uint,
    /// Raw input dptr (unused)
    pub raw_input_dptr: c_ulonglong,
    /// Raw input pitch (unused)
    pub raw_input_pitch: c_uint,
    /// Raw input format (unused)
    pub raw_input_format: c_uint,
    /// Raw output dptr (unused)
    pub raw_output_dptr: c_ulonglong,
    /// Raw output pitch (unused)
    pub raw_output_pitch: c_uint,
    /// Reserved
    pub Reserved1: c_uint,
    /// Output stream
    pub output_stream: *mut c_void,
    /// Reserved
    pub Reserved: [c_uint; 46],
    /// Histogram dptr
    pub histogram_dptr: *mut c_ulonglong,
    /// Reserved
    pub Reserved2: *mut c_void,
}

impl Default for CUVIDPROCPARAMS {
    fn default() -> Self {
        Self {
            progressive_frame: 1,
            second_field: 0,
            top_field_first: 0,
            unpaired_field: 0,
            reserved_flags: 0,
            reserved_zero: 0,
            raw_input_dptr: 0,
            raw_input_pitch: 0,
            raw_input_format: 0,
            raw_output_dptr: 0,
            raw_output_pitch: 0,
            Reserved1: 0,
            output_stream: std::ptr::null_mut(),
            Reserved: [0; 46],
            histogram_dptr: std::ptr::null_mut(),
            Reserved2: std::ptr::null_mut(),
        }
    }
}

impl Default for CUVIDDECODECAPS {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

impl Default for CUVIDDECODECREATEINFO {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

impl Default for CUVIDPICPARAMS {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

//! Platform-independent CMAF (Common Media Application Format) muxer for H.264 and AV1 video streams.
//!
//! This module provides a pure-Rust CMAF muxer, H.264 Annex B parser, and AV1 OBU parser
//! suitable for:
//! - Live streaming (DASH/HLS)
//! - Media Source Extensions (MSE) in browsers
//! - Low-latency video delivery
//!
//! Both NVENC (Linux) and VideoToolbox (macOS) encoders can use this module to produce
//! identical CMAF wire format over MoQ.
//!
//! # CMAF Structure
//!
//! ```text
//! Initialization Segment:
//!   ftyp (file type)
//!   moov (movie header with track info, SPS/PPS or av1C)
//!
//! Media Segments:
//!   styp (segment type)
//!   moof (movie fragment header)
//!   mdat (media data - encoded NAL units or AV1 OBUs)
//! ```

/// H.264 NAL unit type constants.
pub mod nal_unit_type {
    /// Non-IDR slice (P/B frame)
    pub const NON_IDR_SLICE: u8 = 1;
    /// IDR slice (keyframe)
    pub const IDR_SLICE: u8 = 5;
    /// Supplemental enhancement information
    pub const SEI: u8 = 6;
    /// Sequence parameter set
    pub const SPS: u8 = 7;
    /// Picture parameter set
    pub const PPS: u8 = 8;
}

/// A single H.264 NAL unit.
#[derive(Debug, Clone)]
pub struct NalUnit {
    /// The raw NAL unit data (without length prefix, without start code).
    pub data: Vec<u8>,
    /// NAL unit type (from first byte & 0x1F).
    pub nal_type: u8,
}

impl NalUnit {
    /// Returns true if this NAL unit is an IDR (keyframe) slice.
    pub fn is_idr(&self) -> bool {
        self.nal_type == nal_unit_type::IDR_SLICE
    }

    /// Returns true if this NAL unit is an SPS.
    pub fn is_sps(&self) -> bool {
        self.nal_type == nal_unit_type::SPS
    }

    /// Returns true if this NAL unit is a PPS.
    pub fn is_pps(&self) -> bool {
        self.nal_type == nal_unit_type::PPS
    }

    /// Returns true if this NAL unit is a video slice (IDR or non-IDR).
    pub fn is_slice(&self) -> bool {
        self.nal_type == nal_unit_type::IDR_SLICE || self.nal_type == nal_unit_type::NON_IDR_SLICE
    }

    /// Convert NAL unit to Annex B format (with 0x00000001 start code).
    pub fn to_annex_b(&self) -> Vec<u8> {
        let mut result = Vec::with_capacity(4 + self.data.len());
        result.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        result.extend_from_slice(&self.data);
        result
    }
}

/// A parsed Annex B frame with separated NAL units.
#[derive(Debug)]
pub struct ParsedFrame {
    /// Slice NAL units (non-SPS/PPS NALs).
    pub nals: Vec<NalUnit>,
    /// SPS data if present (without start code).
    pub sps: Option<Vec<u8>>,
    /// PPS data if present (without start code).
    pub pps: Option<Vec<u8>>,
    /// Whether this frame contains a keyframe (IDR slice).
    pub is_keyframe: bool,
}

/// Parse raw Annex B H.264 data into structured NAL units.
///
/// Splits on 3-byte (0x000001) and 4-byte (0x00000001) start codes,
/// extracts SPS/PPS/slice NALs, and determines keyframe status.
pub fn parse_annex_b(data: &[u8]) -> ParsedFrame {
    let mut nals = Vec::new();
    let mut sps = None;
    let mut pps = None;
    let mut is_keyframe = false;

    // Find all NAL unit boundaries by scanning for start codes
    let mut nal_starts = Vec::new();
    let mut i = 0;
    while i < data.len() {
        if i + 3 < data.len()
            && data[i] == 0
            && data[i + 1] == 0
            && data[i + 2] == 0
            && data[i + 3] == 1
        {
            // 4-byte start code
            nal_starts.push(i + 4);
            i += 4;
        } else if i + 2 < data.len() && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            // 3-byte start code
            nal_starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }

    for (idx, &start) in nal_starts.iter().enumerate() {
        if start >= data.len() {
            continue;
        }

        let end = if idx + 1 < nal_starts.len() {
            // Find the start code position (not the NAL data start) for the next NAL
            let next_start = nal_starts[idx + 1];
            // Back up past the start code to find where this NAL's data ends
            if next_start >= 4
                && data[next_start - 4] == 0
                && data[next_start - 3] == 0
                && data[next_start - 2] == 0
                && data[next_start - 1] == 1
            {
                next_start - 4
            } else if next_start >= 3
                && data[next_start - 3] == 0
                && data[next_start - 2] == 0
                && data[next_start - 1] == 1
            {
                next_start - 3
            } else {
                next_start
            }
        } else {
            data.len()
        };

        if start >= end {
            continue;
        }

        let nal_data = &data[start..end];
        let nal_type = nal_data[0] & 0x1F;

        match nal_type {
            nal_unit_type::SPS => {
                sps = Some(nal_data.to_vec());
            }
            nal_unit_type::PPS => {
                pps = Some(nal_data.to_vec());
            }
            nal_unit_type::IDR_SLICE => {
                is_keyframe = true;
                nals.push(NalUnit {
                    data: nal_data.to_vec(),
                    nal_type,
                });
            }
            nal_unit_type::NON_IDR_SLICE => {
                nals.push(NalUnit {
                    data: nal_data.to_vec(),
                    nal_type,
                });
            }
            _ => {
                // SEI and other NAL types: include as-is
                nals.push(NalUnit {
                    data: nal_data.to_vec(),
                    nal_type,
                });
            }
        }
    }

    ParsedFrame {
        nals,
        sps,
        pps,
        is_keyframe,
    }
}

// ============================================================================
// AV1 OBU (Open Bitstream Unit) parsing
// ============================================================================

/// AV1 OBU type constants.
pub mod obu_type {
    pub const SEQUENCE_HEADER: u8 = 1;
    pub const TEMPORAL_DELIMITER: u8 = 2;
    pub const FRAME_HEADER: u8 = 3;
    pub const TILE_GROUP: u8 = 4;
    pub const FRAME: u8 = 6;
}

/// A parsed AV1 OBU.
#[derive(Debug, Clone)]
pub struct Obu {
    /// OBU type (from header byte).
    pub obu_type: u8,
    /// Complete OBU data including the header.
    pub data: Vec<u8>,
}

/// Read a LEB128 (unsigned) value from data at the given offset.
/// Returns (value, bytes_consumed).
fn read_leb128(data: &[u8], offset: usize) -> (u64, usize) {
    let mut value: u64 = 0;
    let mut bytes_read = 0;
    for i in 0..8 {
        if offset + i >= data.len() {
            break;
        }
        let byte = data[offset + i];
        value |= ((byte & 0x7F) as u64) << (i * 7);
        bytes_read += 1;
        if byte & 0x80 == 0 {
            break;
        }
    }
    (value, bytes_read)
}

/// Parse AV1 bitstream into individual OBUs.
pub fn parse_av1_obus(data: &[u8]) -> Vec<Obu> {
    let mut obus = Vec::new();
    let mut offset = 0;

    while offset < data.len() {
        let header_byte = data[offset];
        let obu_type = (header_byte >> 3) & 0x0F;
        let has_extension = (header_byte >> 2) & 1 == 1;
        let has_size = (header_byte >> 1) & 1 == 1;

        let mut header_size = 1;
        if has_extension {
            header_size += 1;
        }

        if !has_size {
            // No size field — rest of data is this OBU
            obus.push(Obu {
                obu_type,
                data: data[offset..].to_vec(),
            });
            break;
        }

        let (obu_size, leb_bytes) = read_leb128(data, offset + header_size);
        header_size += leb_bytes;

        let total_size = header_size + obu_size as usize;
        let end = (offset + total_size).min(data.len());

        obus.push(Obu {
            obu_type,
            data: data[offset..end].to_vec(),
        });

        offset = end;
    }

    obus
}

/// Extract the Sequence Header OBU from AV1 bitstream data.
/// Returns the complete OBU (header + payload) if found.
pub fn extract_av1_sequence_header(data: &[u8]) -> Option<Vec<u8>> {
    for obu in parse_av1_obus(data) {
        if obu.obu_type == obu_type::SEQUENCE_HEADER {
            return Some(obu.data);
        }
    }
    None
}

/// Parsed AV1 frame info from NVENC output.
#[derive(Debug)]
pub struct ParsedAv1Frame {
    /// Sequence header OBU if present (for keyframes).
    pub sequence_header: Option<Vec<u8>>,
    /// Whether this frame is a keyframe.
    pub is_keyframe: bool,
    /// The raw encoded data (all OBUs).
    pub data: Vec<u8>,
}

/// Parse raw AV1 bitstream from NVENC into structured frame info.
pub fn parse_av1_frame(data: &[u8]) -> ParsedAv1Frame {
    let obus = parse_av1_obus(data);
    let sequence_header = obus
        .iter()
        .find(|o| o.obu_type == obu_type::SEQUENCE_HEADER)
        .map(|o| o.data.clone());
    // A keyframe is indicated by presence of a sequence header (NVENC emits it with keyframes)
    let is_keyframe = sequence_header.is_some();

    ParsedAv1Frame {
        sequence_header,
        is_keyframe,
        data: data.to_vec(),
    }
}

// ============================================================================
// CMAF Muxer
// ============================================================================

/// Configuration for the CMAF muxer.
#[derive(Debug, Clone)]
pub struct CmafConfig {
    /// Target fragment duration in milliseconds.
    /// Fragments are aligned to keyframes, so actual duration may vary.
    pub fragment_duration_ms: u32,
    /// Timescale for timestamps (e.g., 90000 for standard video).
    pub timescale: u32,
}

impl Default for CmafConfig {
    fn default() -> Self {
        Self {
            fragment_duration_ms: 2000,
            timescale: 90000,
        }
    }
}

/// A pending frame waiting to be muxed.
#[derive(Debug, Clone)]
struct PendingFrame {
    /// Encoded NAL unit data (in AVCC format for mdat)
    data: Vec<u8>,
    /// Duration in timescale units
    duration: u32,
    /// Is this a sync sample (keyframe)
    is_sync: bool,
    /// Composition time offset (PTS - DTS)
    composition_offset: i32,
}

/// Fragmented MP4 muxer for H.264 video streams.
pub struct CmafMuxer {
    config: CmafConfig,
    /// Whether initialization segment has been created
    initialized: bool,
    /// Width in pixels
    width: u32,
    /// Height in pixels
    height: u32,
    /// SPS data (without NAL start code)
    sps: Vec<u8>,
    /// PPS data (without NAL start code)
    pps: Vec<u8>,
    /// Pending frames for current fragment
    pending_frames: Vec<PendingFrame>,
    /// Current fragment sequence number
    sequence_number: u32,
    /// Base DTS for current fragment
    fragment_base_dts: i64,
    /// Last frame's DTS
    last_dts: i64,
    /// Track ID
    track_id: u32,
}

impl CmafMuxer {
    /// Create a new CMAF muxer with the given configuration.
    pub fn new(config: CmafConfig) -> Self {
        Self {
            config,
            initialized: false,
            width: 0,
            height: 0,
            sps: Vec::new(),
            pps: Vec::new(),
            pending_frames: Vec::new(),
            sequence_number: 1,
            fragment_base_dts: 0,
            last_dts: 0,
            track_id: 1,
        }
    }

    /// Create the initialization segment (ftyp + moov).
    ///
    /// This must be called once before adding frames. The initialization segment
    /// contains codec configuration (SPS/PPS) and must be sent before any media
    /// segments.
    pub fn create_init_segment(
        &mut self,
        sps: &[u8],
        pps: &[u8],
        width: u32,
        height: u32,
    ) -> Vec<u8> {
        self.sps = sps.to_vec();
        self.pps = pps.to_vec();
        self.width = width;
        self.height = height;
        self.initialized = true;

        let mut buf = Vec::new();

        // ftyp box
        self.write_ftyp(&mut buf);

        // moov box
        self.write_moov(&mut buf);

        buf
    }

    /// Add an encoded frame to the muxer.
    ///
    /// Returns a media segment when enough frames have accumulated or when a
    /// new keyframe arrives after the target fragment duration.
    pub fn add_frame(
        &mut self,
        nal_units: &[NalUnit],
        pts: i64,
        dts: i64,
        duration: u32,
        is_keyframe: bool,
    ) -> Option<Vec<u8>> {
        if !self.initialized {
            return None;
        }

        // Check if we should start a new fragment
        let should_flush = if self.pending_frames.is_empty() {
            false
        } else {
            // Flush if we have a keyframe and exceeded target duration
            let fragment_duration =
                (dts - self.fragment_base_dts) * 1000 / self.config.timescale as i64;
            is_keyframe && fragment_duration >= self.config.fragment_duration_ms as i64
        };

        let segment = if should_flush {
            Some(self.flush_fragment())
        } else {
            None
        };

        // Convert NAL units to AVCC format for mdat
        let data = self.nal_units_to_avcc(nal_units);

        // If this is the first frame in a fragment, record base DTS
        if self.pending_frames.is_empty() {
            self.fragment_base_dts = dts;
        }

        let composition_offset = (pts - dts) as i32;

        self.pending_frames.push(PendingFrame {
            data,
            duration,
            is_sync: is_keyframe,
            composition_offset,
        });

        self.last_dts = dts;

        segment
    }

    /// Flush any remaining frames as a final segment.
    pub fn flush(&mut self) -> Option<Vec<u8>> {
        if self.pending_frames.is_empty() {
            return None;
        }
        Some(self.flush_fragment())
    }

    /// Convert NAL units to AVCC format (length-prefixed).
    fn nal_units_to_avcc(&self, nal_units: &[NalUnit]) -> Vec<u8> {
        let total_size: usize = nal_units
            .iter()
            .filter(|n| n.is_slice()) // Only include video slices
            .map(|n| 4 + n.data.len())
            .sum();

        let mut buf = Vec::with_capacity(total_size);

        for nal in nal_units.iter().filter(|n| n.is_slice()) {
            let len = nal.data.len() as u32;
            buf.extend_from_slice(&len.to_be_bytes());
            buf.extend_from_slice(&nal.data);
        }

        buf
    }

    /// Create a media segment from pending frames.
    fn flush_fragment(&mut self) -> Vec<u8> {
        let mut buf = Vec::new();

        // styp box
        self.write_styp(&mut buf);

        // moof box
        self.write_moof(&mut buf);

        // mdat box
        self.write_mdat(&mut buf);

        self.sequence_number += 1;
        self.pending_frames.clear();

        buf
    }

    // ========================================
    // Box writing helpers
    // ========================================

    fn write_ftyp(&self, buf: &mut Vec<u8>) {
        let brands = [
            b"isom", // ISO Base Media
            b"iso6", // ISO with fragments
            b"cmfc", // CMAF compliant
            b"cmfv", // CMAF video track
            b"avc1", // H.264
            b"mp41", // MP4 v1
        ];

        let size = 8 + 4 + 4 + (brands.len() * 4);
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"ftyp");
        buf.extend_from_slice(b"isom"); // major brand
        buf.extend_from_slice(&0u32.to_be_bytes()); // minor version
        for brand in &brands {
            buf.extend_from_slice(*brand);
        }
    }

    fn write_styp(&self, buf: &mut Vec<u8>) {
        let brands = [
            b"msdh", // Media Segment Data Handler
            b"msix", // Media Segment Index
            b"cmfc", // CMAF compliant
            b"cmfv", // CMAF video track
        ];
        let size = 8 + 4 + 4 + (brands.len() * 4);
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"styp");
        buf.extend_from_slice(b"cmfv"); // major brand (CMAF video)
        buf.extend_from_slice(&0u32.to_be_bytes()); // minor version
        for brand in &brands {
            buf.extend_from_slice(*brand);
        }
    }

    fn write_moov(&self, buf: &mut Vec<u8>) {
        let mut moov_content = Vec::new();

        // mvhd (movie header)
        self.write_mvhd(&mut moov_content);

        // trak (track)
        self.write_trak(&mut moov_content);

        // mvex (movie extends - required for fragmented MP4)
        self.write_mvex(&mut moov_content);

        let size = 8 + moov_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"moov");
        buf.extend_from_slice(&moov_content);
    }

    fn write_mvhd(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();

        content.push(0); // version
        content.extend_from_slice(&[0, 0, 0]); // flags

        content.extend_from_slice(&0u32.to_be_bytes()); // creation time
        content.extend_from_slice(&0u32.to_be_bytes()); // modification time
        content.extend_from_slice(&self.config.timescale.to_be_bytes()); // timescale
        content.extend_from_slice(&0u32.to_be_bytes()); // duration (unknown for live)

        content.extend_from_slice(&0x00010000u32.to_be_bytes()); // rate (1.0)
        content.extend_from_slice(&0x0100u16.to_be_bytes()); // volume (1.0)
        content.extend_from_slice(&[0; 2]); // reserved
        content.extend_from_slice(&[0; 8]); // reserved

        // Matrix (identity)
        let matrix: [u32; 9] = [0x00010000, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000];
        for m in &matrix {
            content.extend_from_slice(&m.to_be_bytes());
        }

        content.extend_from_slice(&[0; 24]); // pre_defined
        content.extend_from_slice(&2u32.to_be_bytes()); // next_track_id

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"mvhd");
        buf.extend_from_slice(&content);
    }

    fn write_trak(&self, buf: &mut Vec<u8>) {
        let mut trak_content = Vec::new();

        self.write_tkhd(&mut trak_content);
        self.write_mdia(&mut trak_content);

        let size = 8 + trak_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"trak");
        buf.extend_from_slice(&trak_content);
    }

    fn write_tkhd(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();

        content.push(0); // version
        content.extend_from_slice(&[0, 0, 3]); // flags (track enabled, in movie)

        content.extend_from_slice(&0u32.to_be_bytes()); // creation time
        content.extend_from_slice(&0u32.to_be_bytes()); // modification time
        content.extend_from_slice(&self.track_id.to_be_bytes()); // track id
        content.extend_from_slice(&0u32.to_be_bytes()); // reserved
        content.extend_from_slice(&0u32.to_be_bytes()); // duration (unknown)

        content.extend_from_slice(&[0; 8]); // reserved
        content.extend_from_slice(&0i16.to_be_bytes()); // layer
        content.extend_from_slice(&0i16.to_be_bytes()); // alternate_group
        content.extend_from_slice(&0i16.to_be_bytes()); // volume (video = 0)
        content.extend_from_slice(&0u16.to_be_bytes()); // reserved

        // Matrix
        let matrix: [u32; 9] = [0x00010000, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000];
        for m in &matrix {
            content.extend_from_slice(&m.to_be_bytes());
        }

        // Width and height as 16.16 fixed point
        content.extend_from_slice(&(self.width << 16).to_be_bytes());
        content.extend_from_slice(&(self.height << 16).to_be_bytes());

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"tkhd");
        buf.extend_from_slice(&content);
    }

    fn write_mdia(&self, buf: &mut Vec<u8>) {
        let mut mdia_content = Vec::new();

        self.write_mdhd(&mut mdia_content);
        self.write_hdlr(&mut mdia_content);
        self.write_minf(&mut mdia_content);

        let size = 8 + mdia_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"mdia");
        buf.extend_from_slice(&mdia_content);
    }

    fn write_mdhd(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();

        content.push(0); // version
        content.extend_from_slice(&[0, 0, 0]); // flags

        content.extend_from_slice(&0u32.to_be_bytes()); // creation time
        content.extend_from_slice(&0u32.to_be_bytes()); // modification time
        content.extend_from_slice(&self.config.timescale.to_be_bytes()); // timescale
        content.extend_from_slice(&0u32.to_be_bytes()); // duration

        content.extend_from_slice(&0x55c4u16.to_be_bytes()); // language (und)
        content.extend_from_slice(&0u16.to_be_bytes()); // pre_defined

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"mdhd");
        buf.extend_from_slice(&content);
    }

    fn write_hdlr(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();

        content.push(0); // version
        content.extend_from_slice(&[0, 0, 0]); // flags
        content.extend_from_slice(&0u32.to_be_bytes()); // pre_defined
        content.extend_from_slice(b"vide"); // handler_type
        content.extend_from_slice(&[0; 12]); // reserved
        content.extend_from_slice(b"VideoHandler\0"); // name

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"hdlr");
        buf.extend_from_slice(&content);
    }

    fn write_minf(&self, buf: &mut Vec<u8>) {
        let mut minf_content = Vec::new();

        self.write_vmhd(&mut minf_content);
        self.write_dinf(&mut minf_content);
        self.write_stbl(&mut minf_content);

        let size = 8 + minf_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"minf");
        buf.extend_from_slice(&minf_content);
    }

    fn write_vmhd(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();

        content.push(0); // version
        content.extend_from_slice(&[0, 0, 1]); // flags
        content.extend_from_slice(&0u16.to_be_bytes()); // graphics_mode
        content.extend_from_slice(&[0; 6]); // opcolor

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"vmhd");
        buf.extend_from_slice(&content);
    }

    fn write_dinf(&self, buf: &mut Vec<u8>) {
        let mut dinf_content = Vec::new();

        // dref box
        let mut dref_content = Vec::new();
        dref_content.push(0); // version
        dref_content.extend_from_slice(&[0, 0, 0]); // flags
        dref_content.extend_from_slice(&1u32.to_be_bytes()); // entry_count

        // url entry (self-contained)
        dref_content.extend_from_slice(&12u32.to_be_bytes()); // size
        dref_content.extend_from_slice(b"url ");
        dref_content.push(0); // version
        dref_content.extend_from_slice(&[0, 0, 1]); // flags (self-contained)

        let dref_size = 8 + dref_content.len();
        dinf_content.extend_from_slice(&(dref_size as u32).to_be_bytes());
        dinf_content.extend_from_slice(b"dref");
        dinf_content.extend_from_slice(&dref_content);

        let size = 8 + dinf_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"dinf");
        buf.extend_from_slice(&dinf_content);
    }

    fn write_stbl(&self, buf: &mut Vec<u8>) {
        let mut stbl_content = Vec::new();

        self.write_stsd(&mut stbl_content);
        self.write_empty_stts(&mut stbl_content);
        self.write_empty_stsc(&mut stbl_content);
        self.write_empty_stsz(&mut stbl_content);
        self.write_empty_stco(&mut stbl_content);

        let size = 8 + stbl_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"stbl");
        buf.extend_from_slice(&stbl_content);
    }

    fn write_stsd(&self, buf: &mut Vec<u8>) {
        let mut stsd_content = Vec::new();

        stsd_content.push(0); // version
        stsd_content.extend_from_slice(&[0, 0, 0]); // flags
        stsd_content.extend_from_slice(&1u32.to_be_bytes()); // entry_count

        // avc1 sample entry
        self.write_avc1(&mut stsd_content);

        let size = 8 + stsd_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"stsd");
        buf.extend_from_slice(&stsd_content);
    }

    fn write_avc1(&self, buf: &mut Vec<u8>) {
        let mut avc1_content = Vec::new();

        avc1_content.extend_from_slice(&[0; 6]); // reserved
        avc1_content.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index

        avc1_content.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
        avc1_content.extend_from_slice(&0u16.to_be_bytes()); // reserved
        avc1_content.extend_from_slice(&[0; 12]); // pre_defined

        avc1_content.extend_from_slice(&(self.width as u16).to_be_bytes());
        avc1_content.extend_from_slice(&(self.height as u16).to_be_bytes());

        avc1_content.extend_from_slice(&0x00480000u32.to_be_bytes()); // horiz resolution 72 dpi
        avc1_content.extend_from_slice(&0x00480000u32.to_be_bytes()); // vert resolution 72 dpi
        avc1_content.extend_from_slice(&0u32.to_be_bytes()); // reserved
        avc1_content.extend_from_slice(&1u16.to_be_bytes()); // frame_count

        // Compressor name (32 bytes)
        let mut compressor = [0u8; 32];
        let name = b"xoq-cmaf";
        compressor[0] = name.len() as u8;
        compressor[1..1 + name.len()].copy_from_slice(name);
        avc1_content.extend_from_slice(&compressor);

        avc1_content.extend_from_slice(&0x0018u16.to_be_bytes()); // depth (24-bit)
        avc1_content.extend_from_slice(&(-1i16).to_be_bytes()); // pre_defined

        // avcC box
        self.write_avcc(&mut avc1_content);

        let size = 8 + avc1_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"avc1");
        buf.extend_from_slice(&avc1_content);
    }

    fn write_avcc(&self, buf: &mut Vec<u8>) {
        let mut avcc_content = Vec::new();

        avcc_content.push(1); // configuration_version

        // Profile, compatibility, and level from SPS
        if self.sps.len() >= 4 {
            avcc_content.push(self.sps[1]); // profile_idc
            avcc_content.push(self.sps[2]); // profile_compatibility
            avcc_content.push(self.sps[3]); // level_idc
        } else {
            avcc_content.extend_from_slice(&[0x64, 0x00, 0x1f]); // High profile, level 3.1
        }

        avcc_content.push(0xFF); // length_size_minus_one (3 = 4 bytes) | reserved (0b111111)

        // SPS
        avcc_content.push(0xE1); // num_sps | reserved (0b111)
        avcc_content.extend_from_slice(&(self.sps.len() as u16).to_be_bytes());
        avcc_content.extend_from_slice(&self.sps);

        // PPS
        avcc_content.push(1); // num_pps
        avcc_content.extend_from_slice(&(self.pps.len() as u16).to_be_bytes());
        avcc_content.extend_from_slice(&self.pps);

        let size = 8 + avcc_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"avcC");
        buf.extend_from_slice(&avcc_content);
    }

    fn write_empty_stts(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();
        content.push(0); // version
        content.extend_from_slice(&[0, 0, 0]); // flags
        content.extend_from_slice(&0u32.to_be_bytes()); // entry_count

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"stts");
        buf.extend_from_slice(&content);
    }

    fn write_empty_stsc(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();
        content.push(0); // version
        content.extend_from_slice(&[0, 0, 0]); // flags
        content.extend_from_slice(&0u32.to_be_bytes()); // entry_count

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"stsc");
        buf.extend_from_slice(&content);
    }

    fn write_empty_stsz(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();
        content.push(0); // version
        content.extend_from_slice(&[0, 0, 0]); // flags
        content.extend_from_slice(&0u32.to_be_bytes()); // sample_size
        content.extend_from_slice(&0u32.to_be_bytes()); // sample_count

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"stsz");
        buf.extend_from_slice(&content);
    }

    fn write_empty_stco(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();
        content.push(0); // version
        content.extend_from_slice(&[0, 0, 0]); // flags
        content.extend_from_slice(&0u32.to_be_bytes()); // entry_count

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"stco");
        buf.extend_from_slice(&content);
    }

    fn write_mvex(&self, buf: &mut Vec<u8>) {
        let mut mvex_content = Vec::new();

        // trex box
        self.write_trex(&mut mvex_content);

        let size = 8 + mvex_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"mvex");
        buf.extend_from_slice(&mvex_content);
    }

    fn write_trex(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();

        content.push(0); // version
        content.extend_from_slice(&[0, 0, 0]); // flags
        content.extend_from_slice(&self.track_id.to_be_bytes()); // track_id
        content.extend_from_slice(&1u32.to_be_bytes()); // default_sample_description_index
        content.extend_from_slice(&0u32.to_be_bytes()); // default_sample_duration
        content.extend_from_slice(&0u32.to_be_bytes()); // default_sample_size
        content.extend_from_slice(&0u32.to_be_bytes()); // default_sample_flags

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"trex");
        buf.extend_from_slice(&content);
    }

    fn write_moof(&self, buf: &mut Vec<u8>) {
        let mut moof_content = Vec::new();

        // mfhd (movie fragment header)
        self.write_mfhd(&mut moof_content);

        // traf (track fragment)
        self.write_traf(&mut moof_content);

        let size = 8 + moof_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"moof");
        buf.extend_from_slice(&moof_content);
    }

    fn write_mfhd(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();

        content.push(0); // version
        content.extend_from_slice(&[0, 0, 0]); // flags
        content.extend_from_slice(&self.sequence_number.to_be_bytes());

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"mfhd");
        buf.extend_from_slice(&content);
    }

    fn write_traf(&self, buf: &mut Vec<u8>) {
        let mut traf_content = Vec::new();

        // tfhd (track fragment header)
        self.write_tfhd(&mut traf_content);

        // tfdt (track fragment decode time)
        self.write_tfdt(&mut traf_content);

        // trun (track run)
        self.write_trun(&mut traf_content, buf.len());

        let size = 8 + traf_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"traf");
        buf.extend_from_slice(&traf_content);
    }

    fn write_tfhd(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();

        content.push(0); // version
                         // flags: default-base-is-moof (0x020000)
        content.extend_from_slice(&[0x02, 0x00, 0x00]);
        content.extend_from_slice(&self.track_id.to_be_bytes());

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"tfhd");
        buf.extend_from_slice(&content);
    }

    fn write_tfdt(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();

        content.push(1); // version (1 for 64-bit time)
        content.extend_from_slice(&[0, 0, 0]); // flags
        content.extend_from_slice(&(self.fragment_base_dts as u64).to_be_bytes());

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"tfdt");
        buf.extend_from_slice(&content);
    }

    fn write_trun(&self, buf: &mut Vec<u8>, _moof_offset: usize) {
        let sample_count = self.pending_frames.len() as u32;

        // Calculate trun size to determine data_offset
        let trun_content_size = 4 + 4 + 4 + (sample_count as usize * 16);
        let trun_size = 8 + trun_content_size;

        // Calculate data_offset from start of moof to start of mdat data
        let tfhd_size = 8 + 8; // version/flags + track_id
        let tfdt_size = 8 + 12; // version/flags + 64-bit time
        let traf_size = 8 + tfhd_size + tfdt_size + trun_size;
        let mfhd_size = 8 + 8;
        let moof_size = 8 + mfhd_size + traf_size;

        // data_offset is from start of moof to first byte of mdat data
        // = moof_size + 8 (mdat header)
        let data_offset = moof_size + 8;

        let mut content = Vec::new();

        content.push(0); // version
                         // flags: data-offset-present (0x01), sample-duration (0x100),
                         //        sample-size (0x200), sample-flags (0x400),
                         //        sample-composition-time-offset (0x800)
        content.extend_from_slice(&[0x00, 0x0F, 0x01]); // all flags
        content.extend_from_slice(&sample_count.to_be_bytes());
        content.extend_from_slice(&(data_offset as u32).to_be_bytes());

        for frame in &self.pending_frames {
            content.extend_from_slice(&frame.duration.to_be_bytes());
            content.extend_from_slice(&(frame.data.len() as u32).to_be_bytes());

            // Sample flags
            let flags = if frame.is_sync {
                0x02000000u32 // depends_on=2 (no other)
            } else {
                0x01010000u32 // depends_on=1 (yes), is_depended_on=1
            };
            content.extend_from_slice(&flags.to_be_bytes());

            content.extend_from_slice(&frame.composition_offset.to_be_bytes());
        }

        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"trun");
        buf.extend_from_slice(&content);
    }

    fn write_mdat(&self, buf: &mut Vec<u8>) {
        let total_data_size: usize = self.pending_frames.iter().map(|f| f.data.len()).sum();
        let size = 8 + total_data_size;

        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"mdat");

        for frame in &self.pending_frames {
            buf.extend_from_slice(&frame.data);
        }
    }

    /// Get the current sequence number.
    pub fn sequence_number(&self) -> u32 {
        self.sequence_number
    }

    /// Check if the muxer has been initialized.
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Get the number of pending frames.
    pub fn pending_frame_count(&self) -> usize {
        self.pending_frames.len()
    }
}

// ============================================================================
// AV1 CMAF Muxer
// ============================================================================

/// Fragmented MP4 muxer for AV1 video streams.
///
/// Produces CMAF-compliant fMP4 segments with `av01` sample entries.
/// Frame data is written directly to mdat as raw AV1 OBUs (no NAL length-prefix conversion).
pub struct Av1CmafMuxer {
    config: CmafConfig,
    initialized: bool,
    width: u32,
    height: u32,
    /// AV1 Sequence Header OBU (for av1C config box)
    sequence_header_obu: Vec<u8>,
    /// Whether the stream is high bit depth (10-bit or 12-bit)
    high_bitdepth: bool,
    pending_frames: Vec<PendingFrame>,
    sequence_number: u32,
    fragment_base_dts: i64,
    last_dts: i64,
    track_id: u32,
}

impl Av1CmafMuxer {
    /// Create a new AV1 CMAF muxer.
    /// Create a new AV1 CMAF muxer. Set `high_bitdepth` to true for 10-bit streams.
    pub fn new(config: CmafConfig) -> Self {
        Self {
            config,
            initialized: false,
            width: 0,
            height: 0,
            sequence_header_obu: Vec::new(),
            high_bitdepth: false,
            pending_frames: Vec::new(),
            sequence_number: 1,
            fragment_base_dts: 0,
            last_dts: 0,
            track_id: 1,
        }
    }

    /// Set high bit depth flag (10-bit encoding). Must be called before create_init_segment.
    pub fn set_high_bitdepth(&mut self, hbd: bool) {
        self.high_bitdepth = hbd;
    }

    /// Create the initialization segment (ftyp + moov with av01/av1C).
    ///
    /// `sequence_header_obu` is the complete Sequence Header OBU from the encoder,
    /// typically extracted from the first keyframe via `extract_av1_sequence_header()`.
    pub fn create_init_segment(
        &mut self,
        sequence_header_obu: &[u8],
        width: u32,
        height: u32,
    ) -> Vec<u8> {
        self.sequence_header_obu = sequence_header_obu.to_vec();
        self.width = width;
        self.height = height;
        self.initialized = true;

        let mut buf = Vec::new();
        self.write_ftyp(&mut buf);
        self.write_moov(&mut buf);
        buf
    }

    /// Add a raw AV1 encoded frame.
    ///
    /// `data` is the raw NVENC AV1 output (complete OBUs for one temporal unit).
    /// Returns a media segment when a fragment boundary is reached.
    pub fn add_frame(
        &mut self,
        data: &[u8],
        pts: i64,
        dts: i64,
        duration: u32,
        is_keyframe: bool,
    ) -> Option<Vec<u8>> {
        if !self.initialized {
            return None;
        }

        let should_flush = if self.pending_frames.is_empty() {
            false
        } else {
            let fragment_duration =
                (dts - self.fragment_base_dts) * 1000 / self.config.timescale as i64;
            is_keyframe && fragment_duration >= self.config.fragment_duration_ms as i64
        };

        let segment = if should_flush {
            Some(self.flush_fragment())
        } else {
            None
        };

        if self.pending_frames.is_empty() {
            self.fragment_base_dts = dts;
        }

        let composition_offset = (pts - dts) as i32;

        // For AV1 in ISOBMFF, each sample is the raw OBU data directly
        self.pending_frames.push(PendingFrame {
            data: data.to_vec(),
            duration,
            is_sync: is_keyframe,
            composition_offset,
        });

        self.last_dts = dts;
        segment
    }

    /// Flush remaining frames as a final segment.
    pub fn flush(&mut self) -> Option<Vec<u8>> {
        if self.pending_frames.is_empty() {
            return None;
        }
        Some(self.flush_fragment())
    }

    fn flush_fragment(&mut self) -> Vec<u8> {
        let mut buf = Vec::new();
        self.write_styp(&mut buf);
        self.write_moof(&mut buf);
        self.write_mdat(&mut buf);
        self.sequence_number += 1;
        self.pending_frames.clear();
        buf
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    pub fn pending_frame_count(&self) -> usize {
        self.pending_frames.len()
    }

    // ========================================
    // AV1 CMAF box writers
    // ========================================

    fn write_ftyp(&self, buf: &mut Vec<u8>) {
        let brands = [b"isom", b"iso6", b"cmfc", b"cmfv", b"av01", b"mp41"];
        let size = 8 + 4 + 4 + (brands.len() * 4);
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"ftyp");
        buf.extend_from_slice(b"isom");
        buf.extend_from_slice(&0u32.to_be_bytes());
        for brand in &brands {
            buf.extend_from_slice(*brand);
        }
    }

    fn write_styp(&self, buf: &mut Vec<u8>) {
        let brands = [b"msdh", b"msix", b"cmfc", b"cmfv"];
        let size = 8 + 4 + 4 + (brands.len() * 4);
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"styp");
        buf.extend_from_slice(b"cmfv");
        buf.extend_from_slice(&0u32.to_be_bytes());
        for brand in &brands {
            buf.extend_from_slice(*brand);
        }
    }

    fn write_moov(&self, buf: &mut Vec<u8>) {
        let mut moov_content = Vec::new();
        self.write_mvhd(&mut moov_content);
        self.write_trak(&mut moov_content);
        self.write_mvex(&mut moov_content);
        let size = 8 + moov_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"moov");
        buf.extend_from_slice(&moov_content);
    }

    fn write_mvhd(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();
        content.push(0);
        content.extend_from_slice(&[0, 0, 0]);
        content.extend_from_slice(&0u32.to_be_bytes());
        content.extend_from_slice(&0u32.to_be_bytes());
        content.extend_from_slice(&self.config.timescale.to_be_bytes());
        content.extend_from_slice(&0u32.to_be_bytes());
        content.extend_from_slice(&0x00010000u32.to_be_bytes());
        content.extend_from_slice(&0x0100u16.to_be_bytes());
        content.extend_from_slice(&[0; 2]);
        content.extend_from_slice(&[0; 8]);
        let matrix: [u32; 9] = [0x00010000, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000];
        for m in &matrix {
            content.extend_from_slice(&m.to_be_bytes());
        }
        content.extend_from_slice(&[0; 24]);
        content.extend_from_slice(&2u32.to_be_bytes());
        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"mvhd");
        buf.extend_from_slice(&content);
    }

    fn write_trak(&self, buf: &mut Vec<u8>) {
        let mut trak_content = Vec::new();
        self.write_tkhd(&mut trak_content);
        self.write_mdia(&mut trak_content);
        let size = 8 + trak_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"trak");
        buf.extend_from_slice(&trak_content);
    }

    fn write_tkhd(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();
        content.push(0);
        content.extend_from_slice(&[0, 0, 3]);
        content.extend_from_slice(&0u32.to_be_bytes());
        content.extend_from_slice(&0u32.to_be_bytes());
        content.extend_from_slice(&self.track_id.to_be_bytes());
        content.extend_from_slice(&0u32.to_be_bytes());
        content.extend_from_slice(&0u32.to_be_bytes());
        content.extend_from_slice(&[0; 8]);
        content.extend_from_slice(&0i16.to_be_bytes());
        content.extend_from_slice(&0i16.to_be_bytes());
        content.extend_from_slice(&0i16.to_be_bytes());
        content.extend_from_slice(&0u16.to_be_bytes());
        let matrix: [u32; 9] = [0x00010000, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000];
        for m in &matrix {
            content.extend_from_slice(&m.to_be_bytes());
        }
        content.extend_from_slice(&(self.width << 16).to_be_bytes());
        content.extend_from_slice(&(self.height << 16).to_be_bytes());
        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"tkhd");
        buf.extend_from_slice(&content);
    }

    fn write_mdia(&self, buf: &mut Vec<u8>) {
        let mut mdia_content = Vec::new();
        self.write_mdhd(&mut mdia_content);
        self.write_hdlr(&mut mdia_content);
        self.write_minf(&mut mdia_content);
        let size = 8 + mdia_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"mdia");
        buf.extend_from_slice(&mdia_content);
    }

    fn write_mdhd(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();
        content.push(0);
        content.extend_from_slice(&[0, 0, 0]);
        content.extend_from_slice(&0u32.to_be_bytes());
        content.extend_from_slice(&0u32.to_be_bytes());
        content.extend_from_slice(&self.config.timescale.to_be_bytes());
        content.extend_from_slice(&0u32.to_be_bytes());
        content.extend_from_slice(&0x55c4u16.to_be_bytes());
        content.extend_from_slice(&0u16.to_be_bytes());
        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"mdhd");
        buf.extend_from_slice(&content);
    }

    fn write_hdlr(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();
        content.push(0);
        content.extend_from_slice(&[0, 0, 0]);
        content.extend_from_slice(&0u32.to_be_bytes());
        content.extend_from_slice(b"vide");
        content.extend_from_slice(&[0; 12]);
        content.extend_from_slice(b"VideoHandler\0");
        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"hdlr");
        buf.extend_from_slice(&content);
    }

    fn write_minf(&self, buf: &mut Vec<u8>) {
        let mut minf_content = Vec::new();
        self.write_vmhd(&mut minf_content);
        self.write_dinf(&mut minf_content);
        self.write_stbl(&mut minf_content);
        let size = 8 + minf_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"minf");
        buf.extend_from_slice(&minf_content);
    }

    fn write_vmhd(&self, buf: &mut Vec<u8>) {
        let mut content = Vec::new();
        content.push(0);
        content.extend_from_slice(&[0, 0, 1]);
        content.extend_from_slice(&0u16.to_be_bytes());
        content.extend_from_slice(&[0; 6]);
        let size = 8 + content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"vmhd");
        buf.extend_from_slice(&content);
    }

    fn write_dinf(&self, buf: &mut Vec<u8>) {
        let mut dinf_content = Vec::new();
        let mut dref_content = Vec::new();
        dref_content.push(0);
        dref_content.extend_from_slice(&[0, 0, 0]);
        dref_content.extend_from_slice(&1u32.to_be_bytes());
        dref_content.extend_from_slice(&12u32.to_be_bytes());
        dref_content.extend_from_slice(b"url ");
        dref_content.push(0);
        dref_content.extend_from_slice(&[0, 0, 1]);
        let dref_size = 8 + dref_content.len();
        dinf_content.extend_from_slice(&(dref_size as u32).to_be_bytes());
        dinf_content.extend_from_slice(b"dref");
        dinf_content.extend_from_slice(&dref_content);
        let size = 8 + dinf_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"dinf");
        buf.extend_from_slice(&dinf_content);
    }

    fn write_stbl(&self, buf: &mut Vec<u8>) {
        let mut stbl_content = Vec::new();
        self.write_stsd(&mut stbl_content);
        // Empty required boxes (data is in fragments)
        for box_type in [b"stts", b"stsc", b"stsz", b"stco"] {
            let mut c = Vec::new();
            c.push(0);
            c.extend_from_slice(&[0, 0, 0]);
            c.extend_from_slice(&0u32.to_be_bytes());
            if *box_type == *b"stsz" {
                c.extend_from_slice(&0u32.to_be_bytes()); // sample_count
            }
            let s = 8 + c.len();
            stbl_content.extend_from_slice(&(s as u32).to_be_bytes());
            stbl_content.extend_from_slice(box_type);
            stbl_content.extend_from_slice(&c);
        }
        let size = 8 + stbl_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"stbl");
        buf.extend_from_slice(&stbl_content);
    }

    fn write_stsd(&self, buf: &mut Vec<u8>) {
        let mut stsd_content = Vec::new();
        stsd_content.push(0);
        stsd_content.extend_from_slice(&[0, 0, 0]);
        stsd_content.extend_from_slice(&1u32.to_be_bytes());
        self.write_av01(&mut stsd_content);
        let size = 8 + stsd_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"stsd");
        buf.extend_from_slice(&stsd_content);
    }

    /// Write av01 sample entry (AV1 equivalent of avc1).
    fn write_av01(&self, buf: &mut Vec<u8>) {
        let mut av01_content = Vec::new();

        av01_content.extend_from_slice(&[0; 6]); // reserved
        av01_content.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
        av01_content.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
        av01_content.extend_from_slice(&0u16.to_be_bytes()); // reserved
        av01_content.extend_from_slice(&[0; 12]); // pre_defined
        av01_content.extend_from_slice(&(self.width as u16).to_be_bytes());
        av01_content.extend_from_slice(&(self.height as u16).to_be_bytes());
        av01_content.extend_from_slice(&0x00480000u32.to_be_bytes()); // horiz res 72dpi
        av01_content.extend_from_slice(&0x00480000u32.to_be_bytes()); // vert res 72dpi
        av01_content.extend_from_slice(&0u32.to_be_bytes()); // reserved
        av01_content.extend_from_slice(&1u16.to_be_bytes()); // frame_count

        // Compressor name (32 bytes)
        let mut compressor = [0u8; 32];
        let name = b"xoq-av1";
        compressor[0] = name.len() as u8;
        compressor[1..1 + name.len()].copy_from_slice(name);
        av01_content.extend_from_slice(&compressor);

        av01_content.extend_from_slice(&0x0018u16.to_be_bytes()); // depth (24-bit)
        av01_content.extend_from_slice(&(-1i16).to_be_bytes()); // pre_defined

        // av1C box (AV1CodecConfigurationRecord)
        self.write_av1c(&mut av01_content);

        let size = 8 + av01_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"av01");
        buf.extend_from_slice(&av01_content);
    }

    /// Write av1C box containing the AV1 codec configuration.
    fn write_av1c(&self, buf: &mut Vec<u8>) {
        let mut av1c_content = Vec::new();

        // AV1CodecConfigurationRecord (4 bytes + configOBUs)
        // Byte 0: marker(1)=1 | version(7)=1
        av1c_content.push(0x81);

        // Parse seq_profile from sequence header OBU payload
        // The OBU header is 1-2 bytes, then payload starts with seq_profile(3 bits)
        let (seq_profile, seq_level_idx, high_bitdepth, twelve_bit, monochrome, chroma_x, chroma_y) =
            self.parse_sequence_header_fields();

        // Byte 1: seq_profile(3) | seq_level_idx_0(5)
        av1c_content.push((seq_profile << 5) | (seq_level_idx & 0x1F));

        // Byte 2: seq_tier_0(1) | high_bitdepth(1) | twelve_bit(1) | monochrome(1) |
        //         chroma_subsampling_x(1) | chroma_subsampling_y(1) | chroma_sample_position(2)
        #[allow(clippy::identity_op)]
        let byte2 = (0u8 << 7) // seq_tier_0 = 0
            | ((high_bitdepth & 1) << 6)
            | ((twelve_bit & 1) << 5)
            | ((monochrome & 1) << 4)
            | ((chroma_x & 1) << 3)
            | ((chroma_y & 1) << 2)
            | 0; // chroma_sample_position = 0 (unknown)
        av1c_content.push(byte2);

        // Byte 3: reserved(3)=0 | initial_presentation_delay_present(1)=0 | reserved(4)=0
        av1c_content.push(0x00);

        // configOBUs: the Sequence Header OBU
        av1c_content.extend_from_slice(&self.sequence_header_obu);

        let size = 8 + av1c_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"av1C");
        buf.extend_from_slice(&av1c_content);
    }

    /// Parse key fields from the stored Sequence Header OBU for av1C.
    fn parse_sequence_header_fields(&self) -> (u8, u8, u8, u8, u8, u8, u8) {
        // Default values (profile 0, level 4.0, 8-bit 4:2:0)
        let mut seq_profile = 0u8;
        let mut seq_level_idx = 8u8; // Level 4.0
        let high_bitdepth = if self.high_bitdepth { 1u8 } else { 0u8 };
        let twelve_bit = 0u8;
        let monochrome = 0u8;
        let mut chroma_x = 1u8;
        let mut chroma_y = 1u8;

        if self.sequence_header_obu.is_empty() {
            return (
                seq_profile,
                seq_level_idx,
                high_bitdepth,
                twelve_bit,
                monochrome,
                chroma_x,
                chroma_y,
            );
        }

        // Skip OBU header to get to payload
        let header_byte = self.sequence_header_obu[0];
        let has_extension = (header_byte >> 2) & 1 == 1;
        let has_size = (header_byte >> 1) & 1 == 1;
        let mut payload_offset = 1;
        if has_extension {
            payload_offset += 1;
        }
        if has_size {
            let (_size, leb_bytes) = read_leb128(&self.sequence_header_obu, payload_offset);
            payload_offset += leb_bytes;
        }

        if payload_offset >= self.sequence_header_obu.len() {
            return (
                seq_profile,
                seq_level_idx,
                high_bitdepth,
                twelve_bit,
                monochrome,
                chroma_x,
                chroma_y,
            );
        }

        // Sequence Header OBU payload is bit-packed
        // seq_profile (3 bits) | still_picture (1 bit) | reduced_still_picture_header (1 bit) | ...
        let payload = &self.sequence_header_obu[payload_offset..];
        if payload.is_empty() {
            return (
                seq_profile,
                seq_level_idx,
                high_bitdepth,
                twelve_bit,
                monochrome,
                chroma_x,
                chroma_y,
            );
        }

        seq_profile = (payload[0] >> 5) & 0x07;
        let _still_picture = (payload[0] >> 4) & 1;
        let reduced_still_picture_header = (payload[0] >> 3) & 1;

        if reduced_still_picture_header == 1 && payload.len() > 1 {
            // seq_level_idx[0] is next 5 bits (bits 5-9)
            seq_level_idx = ((payload[0] & 0x07) << 2) | (payload[1] >> 6);
        } else if payload.len() > 2 {
            // Non-reduced header: bits 5-6 are timing_info_present(1) + initial_display_delay_present(1),
            // bits 7-11 are operating_points_cnt_minus_1(5), bits 12-23 are operating_point_idc[0](12),
            // bits 24-28 are seq_level_idx[0](5).
            // Bit 5 = timing_info_present_flag
            let timing_info_present = (payload[0] >> 2) & 1;
            if timing_info_present == 0 {
                // Bit 6 = initial_display_delay_present_flag
                // Bits 7-11 = operating_points_cnt_minus_1 (5 bits)
                // Bits 12-23 = operating_point_idc[0] (12 bits)
                // Bits 24-28 = seq_level_idx[0] (5 bits)
                // Byte layout: payload[0] bits [2:0] + payload[1] bits [7:0] + payload[2] bits [7:0] + payload[3] bits [7:0]
                // Bit 5 (timing) is payload[0] bit 2, bit 6 (iddp) is payload[0] bit 1,
                // bits 7-11 (op_cnt) = payload[0] bit 0 + payload[1] bits [7:4]
                // bits 12-23 (op_idc) = payload[1] bits [3:0] + payload[2] bits [7:0]
                // bits 24-28 (level) = payload[3] bits [7:3]
                if payload.len() > 3 {
                    seq_level_idx = (payload[3] >> 3) & 0x1F;
                }
            }
            // If timing_info is present, we can't easily skip it; fall back to default
        }

        // Set bitdepth fields based on profile
        // Profile 0: 8-bit or 10-bit 4:2:0
        // Profile 2: can do 8/10/12-bit
        // We know our config: P010 = 10-bit 4:2:0, ABGR = 8-bit
        // These are set by caller via create_init_segment params
        // For safety, use conservative defaults
        if seq_profile == 0 {
            chroma_x = 1;
            chroma_y = 1;
        }

        (
            seq_profile,
            seq_level_idx,
            high_bitdepth,
            twelve_bit,
            monochrome,
            chroma_x,
            chroma_y,
        )
    }

    fn write_mvex(&self, buf: &mut Vec<u8>) {
        let mut mvex_content = Vec::new();
        // trex box
        let mut trex_content = Vec::new();
        trex_content.push(0);
        trex_content.extend_from_slice(&[0, 0, 0]);
        trex_content.extend_from_slice(&self.track_id.to_be_bytes());
        trex_content.extend_from_slice(&1u32.to_be_bytes());
        trex_content.extend_from_slice(&0u32.to_be_bytes());
        trex_content.extend_from_slice(&0u32.to_be_bytes());
        trex_content.extend_from_slice(&0u32.to_be_bytes());
        let trex_size = 8 + trex_content.len();
        mvex_content.extend_from_slice(&(trex_size as u32).to_be_bytes());
        mvex_content.extend_from_slice(b"trex");
        mvex_content.extend_from_slice(&trex_content);
        let size = 8 + mvex_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"mvex");
        buf.extend_from_slice(&mvex_content);
    }

    fn write_moof(&self, buf: &mut Vec<u8>) {
        let mut moof_content = Vec::new();

        // mfhd
        let mut mfhd_content = Vec::new();
        mfhd_content.push(0);
        mfhd_content.extend_from_slice(&[0, 0, 0]);
        mfhd_content.extend_from_slice(&self.sequence_number.to_be_bytes());
        let mfhd_size = 8 + mfhd_content.len();
        moof_content.extend_from_slice(&(mfhd_size as u32).to_be_bytes());
        moof_content.extend_from_slice(b"mfhd");
        moof_content.extend_from_slice(&mfhd_content);

        // traf
        self.write_traf(&mut moof_content, buf.len());

        let size = 8 + moof_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"moof");
        buf.extend_from_slice(&moof_content);
    }

    fn write_traf(&self, buf: &mut Vec<u8>, _outer_offset: usize) {
        let mut traf_content = Vec::new();

        // tfhd
        let mut tfhd_content = Vec::new();
        tfhd_content.push(0);
        tfhd_content.extend_from_slice(&[0x02, 0x00, 0x00]); // default-base-is-moof
        tfhd_content.extend_from_slice(&self.track_id.to_be_bytes());
        let tfhd_size = 8 + tfhd_content.len();
        traf_content.extend_from_slice(&(tfhd_size as u32).to_be_bytes());
        traf_content.extend_from_slice(b"tfhd");
        traf_content.extend_from_slice(&tfhd_content);

        // tfdt
        let mut tfdt_content = Vec::new();
        tfdt_content.push(1); // version 1 for 64-bit time
        tfdt_content.extend_from_slice(&[0, 0, 0]);
        tfdt_content.extend_from_slice(&(self.fragment_base_dts as u64).to_be_bytes());
        let tfdt_size = 8 + tfdt_content.len();
        traf_content.extend_from_slice(&(tfdt_size as u32).to_be_bytes());
        traf_content.extend_from_slice(b"tfdt");
        traf_content.extend_from_slice(&tfdt_content);

        // trun - calculate sizes for data_offset
        let sample_count = self.pending_frames.len() as u32;
        let trun_content_size = 4 + 4 + 4 + (sample_count as usize * 16);
        let trun_size = 8 + trun_content_size;

        let traf_size = 8 + tfhd_size + tfdt_size + trun_size;
        let mfhd_size = 8 + 8; // version/flags + seq_number
        let moof_size = 8 + mfhd_size + traf_size;
        let data_offset = moof_size + 8; // +8 for mdat header

        let mut trun_content = Vec::new();
        trun_content.push(0);
        trun_content.extend_from_slice(&[0x00, 0x0F, 0x01]); // all flags
        trun_content.extend_from_slice(&sample_count.to_be_bytes());
        trun_content.extend_from_slice(&(data_offset as u32).to_be_bytes());

        for frame in &self.pending_frames {
            trun_content.extend_from_slice(&frame.duration.to_be_bytes());
            trun_content.extend_from_slice(&(frame.data.len() as u32).to_be_bytes());
            let flags = if frame.is_sync {
                0x02000000u32
            } else {
                0x01010000u32
            };
            trun_content.extend_from_slice(&flags.to_be_bytes());
            trun_content.extend_from_slice(&frame.composition_offset.to_be_bytes());
        }

        traf_content.extend_from_slice(&(trun_size as u32).to_be_bytes());
        traf_content.extend_from_slice(b"trun");
        traf_content.extend_from_slice(&trun_content);

        let size = 8 + traf_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"traf");
        buf.extend_from_slice(&traf_content);
    }

    fn write_mdat(&self, buf: &mut Vec<u8>) {
        let total_data_size: usize = self.pending_frames.iter().map(|f| f.data.len()).sum();
        let size = 8 + total_data_size;
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"mdat");
        for frame in &self.pending_frames {
            buf.extend_from_slice(&frame.data);
        }
    }
}

// ============================================================================
// fMP4 Box Parser — for parsing incoming CMAF init/media segments
// ============================================================================

/// Parsed box header from an fMP4 byte stream.
#[derive(Debug, Clone)]
pub struct BoxHeader {
    pub box_type: [u8; 4],
    pub offset: usize,
    pub size: usize,
    pub header_size: usize,
}

impl BoxHeader {
    /// Returns the content bytes (after the header) within the given data slice.
    pub fn content<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        let start = self.offset + self.header_size;
        let end = (self.offset + self.size).min(data.len());
        if start >= end {
            &[]
        } else {
            &data[start..end]
        }
    }
}

/// Iterate over top-level boxes in a byte slice.
pub fn iter_boxes(data: &[u8]) -> Vec<BoxHeader> {
    let mut boxes = Vec::new();
    let mut offset = 0;
    while offset + 8 <= data.len() {
        let size = u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        let box_type: [u8; 4] = [
            data[offset + 4],
            data[offset + 5],
            data[offset + 6],
            data[offset + 7],
        ];
        if size < 8 {
            break;
        }
        boxes.push(BoxHeader {
            box_type,
            offset,
            size: size.min(data.len() - offset),
            header_size: 8,
        });
        offset += size;
    }
    boxes
}

/// Find a specific box by type (non-recursive, searches top-level only).
pub fn find_box(data: &[u8], box_type: &[u8; 4]) -> Option<BoxHeader> {
    iter_boxes(data)
        .into_iter()
        .find(|b| &b.box_type == box_type)
}

/// Find a box by navigating a path of nested container boxes.
/// Returns the content bytes of the final box in the path.
pub fn find_box_path<'a>(data: &'a [u8], path: &[&[u8; 4]]) -> Option<&'a [u8]> {
    if path.is_empty() {
        return Some(data);
    }
    let header = find_box(data, path[0])?;
    let content = header.content(data);
    if path.len() == 1 {
        Some(content)
    } else {
        find_box_path(content, &path[1..])
    }
}

/// Parsed information from a CMAF init segment (ftyp + moov).
#[derive(Debug, Clone)]
pub struct ParsedInitSegment {
    pub av1c_config: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub timescale: u32,
}

/// Parse a CMAF init segment to extract codec config, dimensions, and timescale.
pub fn parse_cmaf_init(data: &[u8]) -> anyhow::Result<ParsedInitSegment> {
    // Navigate: moov → trak → mdia → minf → stbl → stsd → av01 → av1C
    let stsd_content = find_box_path(
        data,
        &[b"moov", b"trak", b"mdia", b"minf", b"stbl", b"stsd"],
    )
    .ok_or_else(|| anyhow::anyhow!("Could not find stsd box in init segment"))?;

    // stsd has 8 bytes of version/flags + entry_count before the sample entry
    if stsd_content.len() < 8 {
        anyhow::bail!("stsd content too short");
    }
    let sample_entry_data = &stsd_content[8..]; // skip version(4) + entry_count(4)

    // The sample entry (av01) is a box itself
    let av01_header = find_box(sample_entry_data, b"av01")
        .ok_or_else(|| anyhow::anyhow!("Could not find av01 sample entry"))?;
    let av01_content = av01_header.content(sample_entry_data);

    // av01 sample entry structure:
    //   6B reserved + 2B data_ref_idx + 2B pre_defined + 2B reserved + 12B pre_defined
    //   + 2B width + 2B height + 4B h_res + 4B v_res + 4B reserved + 2B frame_count
    //   + 32B compressor + 2B depth + 2B pre_defined = 78 bytes
    // Then child boxes (av1C, etc.)
    if av01_content.len() < 78 {
        anyhow::bail!("av01 content too short: {} bytes", av01_content.len());
    }
    let width = u16::from_be_bytes([av01_content[24], av01_content[25]]) as u32;
    let height = u16::from_be_bytes([av01_content[26], av01_content[27]]) as u32;

    // Find av1C in the remaining data after the fixed fields
    let av1c_area = &av01_content[78..];
    let av1c_header =
        find_box(av1c_area, b"av1C").ok_or_else(|| anyhow::anyhow!("Could not find av1C box"))?;
    let av1c_config = av1c_header.content(av1c_area).to_vec();

    // Navigate: moov → trak → mdia → mdhd for timescale
    let mdhd_content = find_box_path(data, &[b"moov", b"trak", b"mdia", b"mdhd"])
        .ok_or_else(|| anyhow::anyhow!("Could not find mdhd box"))?;
    // mdhd v0: version(1) + flags(3) + creation(4) + modification(4) + timescale(4)
    let timescale = if mdhd_content.len() >= 16 {
        u32::from_be_bytes([
            mdhd_content[12],
            mdhd_content[13],
            mdhd_content[14],
            mdhd_content[15],
        ])
    } else {
        90000
    };

    Ok(ParsedInitSegment {
        av1c_config,
        width,
        height,
        timescale,
    })
}

/// A single sample entry from a trun box.
#[derive(Debug, Clone)]
pub struct SampleEntry {
    pub duration: u32,
    pub size: u32,
    pub flags: u32,
    pub composition_offset: i32,
}

/// Parsed information from a CMAF media segment (styp + moof + mdat).
#[derive(Debug, Clone)]
pub struct ParsedMediaSegment {
    pub sequence_number: u32,
    pub base_decode_time: u64,
    pub samples: Vec<SampleEntry>,
    pub mdat_payload: Vec<u8>,
}

/// Parse a CMAF media segment to extract fragment info and raw mdat payload.
pub fn parse_cmaf_media_segment(data: &[u8]) -> anyhow::Result<ParsedMediaSegment> {
    // Find moof box
    let moof_header =
        find_box(data, b"moof").ok_or_else(|| anyhow::anyhow!("Could not find moof box"))?;
    let moof_content = moof_header.content(data);

    // Parse mfhd for sequence_number
    let mfhd_content = find_box(moof_content, b"mfhd")
        .ok_or_else(|| anyhow::anyhow!("Could not find mfhd box"))?;
    let mfhd = mfhd_content.content(moof_content);
    let sequence_number = if mfhd.len() >= 8 {
        u32::from_be_bytes([mfhd[4], mfhd[5], mfhd[6], mfhd[7]])
    } else {
        0
    };

    // Parse traf
    let traf_header = find_box(moof_content, b"traf")
        .ok_or_else(|| anyhow::anyhow!("Could not find traf box"))?;
    let traf_content = traf_header.content(moof_content);

    // Parse tfdt for base_decode_time
    let tfdt_header = find_box(traf_content, b"tfdt")
        .ok_or_else(|| anyhow::anyhow!("Could not find tfdt box"))?;
    let tfdt = tfdt_header.content(traf_content);
    let base_decode_time = if !tfdt.is_empty() && tfdt[0] == 1 {
        // version 1: 64-bit
        if tfdt.len() >= 12 {
            u64::from_be_bytes([
                tfdt[4], tfdt[5], tfdt[6], tfdt[7], tfdt[8], tfdt[9], tfdt[10], tfdt[11],
            ])
        } else {
            0
        }
    } else {
        // version 0: 32-bit
        if tfdt.len() >= 8 {
            u32::from_be_bytes([tfdt[4], tfdt[5], tfdt[6], tfdt[7]]) as u64
        } else {
            0
        }
    };

    // Parse trun for samples
    let trun_header = find_box(traf_content, b"trun")
        .ok_or_else(|| anyhow::anyhow!("Could not find trun box"))?;
    let trun = trun_header.content(traf_content);
    // trun: version(1) + flags(3) + sample_count(4) + [data_offset(4)] + per-sample entries
    if trun.len() < 8 {
        anyhow::bail!("trun content too short");
    }
    let trun_flags = u32::from_be_bytes([0, trun[1], trun[2], trun[3]]);
    let sample_count = u32::from_be_bytes([trun[4], trun[5], trun[6], trun[7]]);

    let mut offset = 8;
    if trun_flags & 0x01 != 0 {
        offset += 4; // data_offset present
    }
    if trun_flags & 0x04 != 0 {
        offset += 4; // first_sample_flags present
    }

    let has_duration = trun_flags & 0x100 != 0;
    let has_size = trun_flags & 0x200 != 0;
    let has_flags = trun_flags & 0x400 != 0;
    let has_cts_offset = trun_flags & 0x800 != 0;

    let mut samples = Vec::with_capacity(sample_count as usize);
    for _ in 0..sample_count {
        let duration = if has_duration && offset + 4 <= trun.len() {
            let v = u32::from_be_bytes([
                trun[offset],
                trun[offset + 1],
                trun[offset + 2],
                trun[offset + 3],
            ]);
            offset += 4;
            v
        } else {
            0
        };
        let size = if has_size && offset + 4 <= trun.len() {
            let v = u32::from_be_bytes([
                trun[offset],
                trun[offset + 1],
                trun[offset + 2],
                trun[offset + 3],
            ]);
            offset += 4;
            v
        } else {
            0
        };
        let flags = if has_flags && offset + 4 <= trun.len() {
            let v = u32::from_be_bytes([
                trun[offset],
                trun[offset + 1],
                trun[offset + 2],
                trun[offset + 3],
            ]);
            offset += 4;
            v
        } else {
            0
        };
        let composition_offset = if has_cts_offset && offset + 4 <= trun.len() {
            let v = i32::from_be_bytes([
                trun[offset],
                trun[offset + 1],
                trun[offset + 2],
                trun[offset + 3],
            ]);
            offset += 4;
            v
        } else {
            0
        };
        samples.push(SampleEntry {
            duration,
            size,
            flags,
            composition_offset,
        });
    }

    // Find mdat box and extract payload
    let mdat_header =
        find_box(data, b"mdat").ok_or_else(|| anyhow::anyhow!("Could not find mdat box"))?;
    let mdat_payload = mdat_header.content(data).to_vec();

    Ok(ParsedMediaSegment {
        sequence_number,
        base_decode_time,
        samples,
        mdat_payload,
    })
}

// ============================================================================
// Multi-Track fMP4 Writer — for recording multiple tracks into one file
// ============================================================================

/// Configuration for a single track in a multi-track recording.
#[derive(Debug, Clone)]
pub struct TrackConfig {
    pub track_id: u32,
    pub timescale: u32,
    /// Handler type: b"vide" for video, b"meta" for metadata
    pub handler: [u8; 4],
    /// For video: av1C config bytes. For metadata: empty.
    pub codec_config: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Whether this is a high-bitdepth (10-bit) track
    pub high_bitdepth: bool,
}

/// Fragment data for one track within a multi-track moof.
#[derive(Debug, Clone)]
pub struct TrackFragment {
    pub track_id: u32,
    pub base_decode_time: u64,
    pub samples: Vec<SampleEntry>,
    pub data: Vec<u8>,
}

/// Multi-track fMP4 writer for recording.
pub struct MultiTrackRecorder {
    sequence_number: u32,
    tracks: Vec<TrackConfig>,
}

impl MultiTrackRecorder {
    pub fn new(tracks: Vec<TrackConfig>) -> Self {
        Self {
            sequence_number: 1,
            tracks,
        }
    }

    /// Write the init segment (ftyp + multi-track moov).
    pub fn write_init_segment(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        self.write_ftyp(&mut buf);
        self.write_moov(&mut buf);
        buf
    }

    /// Write a fragment containing data for one or more tracks.
    pub fn write_fragment(&mut self, fragments: &[TrackFragment]) -> Vec<u8> {
        if fragments.is_empty() {
            return Vec::new();
        }
        let mut buf = Vec::new();
        self.write_multi_moof_mdat(&mut buf, fragments);
        self.sequence_number += 1;
        buf
    }

    fn write_ftyp(&self, buf: &mut Vec<u8>) {
        let brands: &[&[u8; 4]] = &[b"isom", b"iso6", b"cmfc", b"av01", b"mp41"];
        let size = 8 + 4 + 4 + (brands.len() * 4);
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"ftyp");
        buf.extend_from_slice(b"isom");
        buf.extend_from_slice(&0u32.to_be_bytes());
        for brand in brands {
            buf.extend_from_slice(*brand);
        }
    }

    fn write_moov(&self, buf: &mut Vec<u8>) {
        let mut moov_content = Vec::new();

        // mvhd
        self.write_mvhd(&mut moov_content);

        // One trak per track
        for track in &self.tracks {
            self.write_trak(&mut moov_content, track);
        }

        // mvex with one trex per track
        self.write_mvex(&mut moov_content);

        let size = 8 + moov_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"moov");
        buf.extend_from_slice(&moov_content);
    }

    fn write_mvhd(&self, buf: &mut Vec<u8>) {
        let mut c = Vec::new();
        c.push(0); // version
        c.extend_from_slice(&[0, 0, 0]); // flags
        c.extend_from_slice(&0u32.to_be_bytes()); // creation_time
        c.extend_from_slice(&0u32.to_be_bytes()); // modification_time
                                                  // Use the first track's timescale for the movie header
        let ts = self.tracks.first().map(|t| t.timescale).unwrap_or(90000);
        c.extend_from_slice(&ts.to_be_bytes());
        c.extend_from_slice(&0u32.to_be_bytes()); // duration
        c.extend_from_slice(&0x00010000u32.to_be_bytes()); // rate 1.0
        c.extend_from_slice(&0x0100u16.to_be_bytes()); // volume 1.0
        c.extend_from_slice(&[0; 2]); // reserved
        c.extend_from_slice(&[0; 8]); // reserved
        let matrix: [u32; 9] = [0x00010000, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000];
        for m in &matrix {
            c.extend_from_slice(&m.to_be_bytes());
        }
        c.extend_from_slice(&[0; 24]); // pre_defined
        let next_track_id = self.tracks.len() as u32 + 1;
        c.extend_from_slice(&next_track_id.to_be_bytes());
        let size = 8 + c.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"mvhd");
        buf.extend_from_slice(&c);
    }

    fn write_trak(&self, buf: &mut Vec<u8>, track: &TrackConfig) {
        let mut trak_content = Vec::new();
        self.write_tkhd(&mut trak_content, track);
        self.write_mdia(&mut trak_content, track);
        let size = 8 + trak_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"trak");
        buf.extend_from_slice(&trak_content);
    }

    fn write_tkhd(&self, buf: &mut Vec<u8>, track: &TrackConfig) {
        let mut c = Vec::new();
        c.push(0); // version
        c.extend_from_slice(&[0, 0, 3]); // flags (enabled, in movie)
        c.extend_from_slice(&0u32.to_be_bytes()); // creation_time
        c.extend_from_slice(&0u32.to_be_bytes()); // modification_time
        c.extend_from_slice(&track.track_id.to_be_bytes());
        c.extend_from_slice(&0u32.to_be_bytes()); // reserved
        c.extend_from_slice(&0u32.to_be_bytes()); // duration
        c.extend_from_slice(&[0; 8]); // reserved
        c.extend_from_slice(&0i16.to_be_bytes()); // layer
        c.extend_from_slice(&0i16.to_be_bytes()); // alternate_group
                                                  // volume: 0 for video, 0 for metadata
        c.extend_from_slice(&0i16.to_be_bytes());
        c.extend_from_slice(&0u16.to_be_bytes()); // reserved
        let matrix: [u32; 9] = [0x00010000, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000];
        for m in &matrix {
            c.extend_from_slice(&m.to_be_bytes());
        }
        c.extend_from_slice(&(track.width << 16).to_be_bytes());
        c.extend_from_slice(&(track.height << 16).to_be_bytes());
        let size = 8 + c.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"tkhd");
        buf.extend_from_slice(&c);
    }

    fn write_mdia(&self, buf: &mut Vec<u8>, track: &TrackConfig) {
        let mut mdia_content = Vec::new();

        // mdhd
        {
            let mut c = Vec::new();
            c.push(0); // version
            c.extend_from_slice(&[0, 0, 0]);
            c.extend_from_slice(&0u32.to_be_bytes());
            c.extend_from_slice(&0u32.to_be_bytes());
            c.extend_from_slice(&track.timescale.to_be_bytes());
            c.extend_from_slice(&0u32.to_be_bytes());
            c.extend_from_slice(&0x55c4u16.to_be_bytes()); // language: und
            c.extend_from_slice(&0u16.to_be_bytes());
            let s = 8 + c.len();
            mdia_content.extend_from_slice(&(s as u32).to_be_bytes());
            mdia_content.extend_from_slice(b"mdhd");
            mdia_content.extend_from_slice(&c);
        }

        // hdlr
        {
            let mut c = Vec::new();
            c.push(0);
            c.extend_from_slice(&[0, 0, 0]);
            c.extend_from_slice(&0u32.to_be_bytes()); // pre_defined
            c.extend_from_slice(&track.handler);
            c.extend_from_slice(&[0; 12]); // reserved
            let name = if &track.handler == b"vide" {
                b"VideoHandler\0"
            } else {
                b"MetaHandler\0\0"
            };
            c.extend_from_slice(name);
            let s = 8 + c.len();
            mdia_content.extend_from_slice(&(s as u32).to_be_bytes());
            mdia_content.extend_from_slice(b"hdlr");
            mdia_content.extend_from_slice(&c);
        }

        // minf
        {
            let mut minf_content = Vec::new();

            // vmhd or nmhd
            if &track.handler == b"vide" {
                let mut c = Vec::new();
                c.push(0);
                c.extend_from_slice(&[0, 0, 1]);
                c.extend_from_slice(&0u16.to_be_bytes());
                c.extend_from_slice(&[0; 6]);
                let s = 8 + c.len();
                minf_content.extend_from_slice(&(s as u32).to_be_bytes());
                minf_content.extend_from_slice(b"vmhd");
                minf_content.extend_from_slice(&c);
            } else {
                // nmhd (null media header) for metadata tracks
                let mut c = Vec::new();
                c.push(0);
                c.extend_from_slice(&[0, 0, 0]);
                let s = 8 + c.len();
                minf_content.extend_from_slice(&(s as u32).to_be_bytes());
                minf_content.extend_from_slice(b"nmhd");
                minf_content.extend_from_slice(&c);
            }

            // dinf
            {
                let mut dinf_content = Vec::new();
                let mut dref_content = Vec::new();
                dref_content.push(0);
                dref_content.extend_from_slice(&[0, 0, 0]);
                dref_content.extend_from_slice(&1u32.to_be_bytes());
                dref_content.extend_from_slice(&12u32.to_be_bytes());
                dref_content.extend_from_slice(b"url ");
                dref_content.push(0);
                dref_content.extend_from_slice(&[0, 0, 1]);
                let dref_s = 8 + dref_content.len();
                dinf_content.extend_from_slice(&(dref_s as u32).to_be_bytes());
                dinf_content.extend_from_slice(b"dref");
                dinf_content.extend_from_slice(&dref_content);
                let s = 8 + dinf_content.len();
                minf_content.extend_from_slice(&(s as u32).to_be_bytes());
                minf_content.extend_from_slice(b"dinf");
                minf_content.extend_from_slice(&dinf_content);
            }

            // stbl
            {
                let mut stbl_content = Vec::new();
                self.write_stsd(&mut stbl_content, track);
                // Empty required boxes
                for box_type in [b"stts", b"stsc", b"stsz", b"stco"] {
                    let mut c = Vec::new();
                    c.push(0);
                    c.extend_from_slice(&[0, 0, 0]);
                    c.extend_from_slice(&0u32.to_be_bytes());
                    if *box_type == *b"stsz" {
                        c.extend_from_slice(&0u32.to_be_bytes());
                    }
                    let s = 8 + c.len();
                    stbl_content.extend_from_slice(&(s as u32).to_be_bytes());
                    stbl_content.extend_from_slice(box_type);
                    stbl_content.extend_from_slice(&c);
                }
                let s = 8 + stbl_content.len();
                minf_content.extend_from_slice(&(s as u32).to_be_bytes());
                minf_content.extend_from_slice(b"stbl");
                minf_content.extend_from_slice(&stbl_content);
            }

            let s = 8 + minf_content.len();
            mdia_content.extend_from_slice(&(s as u32).to_be_bytes());
            mdia_content.extend_from_slice(b"minf");
            mdia_content.extend_from_slice(&minf_content);
        }

        let size = 8 + mdia_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"mdia");
        buf.extend_from_slice(&mdia_content);
    }

    fn write_stsd(&self, buf: &mut Vec<u8>, track: &TrackConfig) {
        let mut stsd_content = Vec::new();
        stsd_content.push(0); // version
        stsd_content.extend_from_slice(&[0, 0, 0]); // flags
        stsd_content.extend_from_slice(&1u32.to_be_bytes()); // entry_count

        if &track.handler == b"vide" {
            self.write_av01_entry(&mut stsd_content, track);
        } else {
            self.write_mett_entry(&mut stsd_content, track);
        }

        let size = 8 + stsd_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"stsd");
        buf.extend_from_slice(&stsd_content);
    }

    fn write_av01_entry(&self, buf: &mut Vec<u8>, track: &TrackConfig) {
        let mut av01_content = Vec::new();
        av01_content.extend_from_slice(&[0; 6]); // reserved
        av01_content.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
        av01_content.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
        av01_content.extend_from_slice(&0u16.to_be_bytes()); // reserved
        av01_content.extend_from_slice(&[0; 12]); // pre_defined
        av01_content.extend_from_slice(&(track.width as u16).to_be_bytes());
        av01_content.extend_from_slice(&(track.height as u16).to_be_bytes());
        av01_content.extend_from_slice(&0x00480000u32.to_be_bytes()); // h res 72dpi
        av01_content.extend_from_slice(&0x00480000u32.to_be_bytes()); // v res 72dpi
        av01_content.extend_from_slice(&0u32.to_be_bytes()); // reserved
        av01_content.extend_from_slice(&1u16.to_be_bytes()); // frame_count
        let mut compressor = [0u8; 32];
        let name = b"xoq-rec";
        compressor[0] = name.len() as u8;
        compressor[1..1 + name.len()].copy_from_slice(name);
        av01_content.extend_from_slice(&compressor);
        av01_content.extend_from_slice(&0x0018u16.to_be_bytes()); // depth
        av01_content.extend_from_slice(&(-1i16).to_be_bytes()); // pre_defined

        // av1C box — write raw config bytes from parsed init segment
        let mut av1c_box = Vec::new();
        av1c_box.extend_from_slice(&track.codec_config);
        let av1c_size = 8 + av1c_box.len();
        av01_content.extend_from_slice(&(av1c_size as u32).to_be_bytes());
        av01_content.extend_from_slice(b"av1C");
        av01_content.extend_from_slice(&av1c_box);

        let size = 8 + av01_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"av01");
        buf.extend_from_slice(&av01_content);
    }

    fn write_mett_entry(&self, buf: &mut Vec<u8>, _track: &TrackConfig) {
        // mett (text metadata sample entry)
        let mut mett_content = Vec::new();
        mett_content.extend_from_slice(&[0; 6]); // reserved
        mett_content.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
                                                             // MIME type (null-terminated)
        mett_content.extend_from_slice(b"application/octet-stream\0");

        let size = 8 + mett_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"mett");
        buf.extend_from_slice(&mett_content);
    }

    fn write_mvex(&self, buf: &mut Vec<u8>) {
        let mut mvex_content = Vec::new();
        for track in &self.tracks {
            let mut c = Vec::new();
            c.push(0); // version
            c.extend_from_slice(&[0, 0, 0]); // flags
            c.extend_from_slice(&track.track_id.to_be_bytes());
            c.extend_from_slice(&1u32.to_be_bytes()); // default_sample_description_index
            c.extend_from_slice(&0u32.to_be_bytes()); // default_sample_duration
            c.extend_from_slice(&0u32.to_be_bytes()); // default_sample_size
            c.extend_from_slice(&0u32.to_be_bytes()); // default_sample_flags
            let s = 8 + c.len();
            mvex_content.extend_from_slice(&(s as u32).to_be_bytes());
            mvex_content.extend_from_slice(b"trex");
            mvex_content.extend_from_slice(&c);
        }
        let size = 8 + mvex_content.len();
        buf.extend_from_slice(&(size as u32).to_be_bytes());
        buf.extend_from_slice(b"mvex");
        buf.extend_from_slice(&mvex_content);
    }

    fn write_multi_moof_mdat(&self, buf: &mut Vec<u8>, fragments: &[TrackFragment]) {
        // First, build all traf contents to calculate total moof size for data_offset
        let mut traf_bufs: Vec<Vec<u8>> = Vec::new();

        // mfhd size
        let mfhd_size = 8 + 8; // box header + version/flags(4) + seq_number(4)

        // Build each traf (without data_offset fixup yet — we need total moof size first)
        // We'll build trafs in two passes: first to measure, then to fixup data_offset.

        // Calculate the total mdat payload
        let total_mdat_data: usize = fragments.iter().map(|f| f.data.len()).sum();
        let mdat_header_size = 8;

        // Build traf buffers — each traf's trun data_offset is relative to moof start
        // We need to compute: data_offset = moof_size + mdat_header_size + data_offset_within_mdat
        // So we first build all trafs with placeholder data_offset, then fixup.

        let mut data_offsets: Vec<usize> = Vec::new();
        let mut running_data_offset = 0usize;
        for frag in fragments {
            data_offsets.push(running_data_offset);
            running_data_offset += frag.data.len();
        }

        // Build trafs with placeholder data_offset (0)
        for frag in fragments {
            let mut traf_content = Vec::new();

            // tfhd
            {
                let mut c = Vec::new();
                c.push(0); // version
                c.extend_from_slice(&[0x02, 0x00, 0x00]); // default-base-is-moof
                c.extend_from_slice(&frag.track_id.to_be_bytes());
                let s = 8 + c.len();
                traf_content.extend_from_slice(&(s as u32).to_be_bytes());
                traf_content.extend_from_slice(b"tfhd");
                traf_content.extend_from_slice(&c);
            }

            // tfdt
            {
                let mut c = Vec::new();
                c.push(1); // version 1 (64-bit)
                c.extend_from_slice(&[0, 0, 0]);
                c.extend_from_slice(&frag.base_decode_time.to_be_bytes());
                let s = 8 + c.len();
                traf_content.extend_from_slice(&(s as u32).to_be_bytes());
                traf_content.extend_from_slice(b"tfdt");
                traf_content.extend_from_slice(&c);
            }

            // trun (with placeholder data_offset = 0, will be fixed up)
            {
                let sample_count = frag.samples.len() as u32;
                let mut c = Vec::new();
                c.push(0); // version
                c.extend_from_slice(&[0x00, 0x0F, 0x01]); // flags: data-offset + duration + size + flags + cts
                c.extend_from_slice(&sample_count.to_be_bytes());
                c.extend_from_slice(&0u32.to_be_bytes()); // placeholder data_offset

                for sample in &frag.samples {
                    c.extend_from_slice(&sample.duration.to_be_bytes());
                    c.extend_from_slice(&sample.size.to_be_bytes());
                    c.extend_from_slice(&sample.flags.to_be_bytes());
                    c.extend_from_slice(&sample.composition_offset.to_be_bytes());
                }

                let s = 8 + c.len();
                traf_content.extend_from_slice(&(s as u32).to_be_bytes());
                traf_content.extend_from_slice(b"trun");
                traf_content.extend_from_slice(&c);
            }

            let mut traf_buf = Vec::new();
            let s = 8 + traf_content.len();
            traf_buf.extend_from_slice(&(s as u32).to_be_bytes());
            traf_buf.extend_from_slice(b"traf");
            traf_buf.extend_from_slice(&traf_content);
            traf_bufs.push(traf_buf);
        }

        // Calculate total moof size
        let total_traf_size: usize = traf_bufs.iter().map(|t| t.len()).sum();
        let moof_size = 8 + mfhd_size + total_traf_size;

        // Fix up data_offset in each traf's trun
        // data_offset = moof_size + mdat_header_size + offset_within_mdat
        for (i, traf_buf) in traf_bufs.iter_mut().enumerate() {
            let data_offset = (moof_size + mdat_header_size + data_offsets[i]) as u32;
            // Find the trun box within this traf and patch data_offset
            // traf structure: [8B traf header][tfhd][tfdt][trun]
            // trun data_offset is at: trun_box_start + 8 (box header) + 4 (version+flags) + 4 (sample_count)
            // = trun_box_start + 16
            let mut pos = 8; // skip traf header
            while pos + 8 <= traf_buf.len() {
                let box_size = u32::from_be_bytes([
                    traf_buf[pos],
                    traf_buf[pos + 1],
                    traf_buf[pos + 2],
                    traf_buf[pos + 3],
                ]) as usize;
                let box_type = &traf_buf[pos + 4..pos + 8];
                if box_type == b"trun" {
                    // data_offset is at pos + 8 + 4 + 4 = pos + 16
                    let do_pos = pos + 16;
                    traf_buf[do_pos..do_pos + 4].copy_from_slice(&data_offset.to_be_bytes());
                    break;
                }
                pos += box_size;
            }
        }

        // Write moof
        buf.extend_from_slice(&(moof_size as u32).to_be_bytes());
        buf.extend_from_slice(b"moof");

        // mfhd
        {
            let mut c = Vec::new();
            c.push(0);
            c.extend_from_slice(&[0, 0, 0]);
            c.extend_from_slice(&self.sequence_number.to_be_bytes());
            let s = 8 + c.len();
            buf.extend_from_slice(&(s as u32).to_be_bytes());
            buf.extend_from_slice(b"mfhd");
            buf.extend_from_slice(&c);
        }

        // trafs
        for traf_buf in &traf_bufs {
            buf.extend_from_slice(traf_buf);
        }

        // mdat
        let mdat_size = mdat_header_size + total_mdat_data;
        buf.extend_from_slice(&(mdat_size as u32).to_be_bytes());
        buf.extend_from_slice(b"mdat");
        for frag in fragments {
            buf.extend_from_slice(&frag.data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nal_unit_types() {
        let idr = NalUnit {
            data: vec![0x65, 0x00],
            nal_type: nal_unit_type::IDR_SLICE,
        };
        assert!(idr.is_idr());
        assert!(idr.is_slice());
        assert!(!idr.is_sps());
        assert!(!idr.is_pps());

        let non_idr = NalUnit {
            data: vec![0x41, 0x00],
            nal_type: nal_unit_type::NON_IDR_SLICE,
        };
        assert!(!non_idr.is_idr());
        assert!(non_idr.is_slice());

        let sps = NalUnit {
            data: vec![0x67, 0x64, 0x00, 0x1f],
            nal_type: nal_unit_type::SPS,
        };
        assert!(sps.is_sps());
        assert!(!sps.is_slice());

        let pps = NalUnit {
            data: vec![0x68, 0xee, 0x3c],
            nal_type: nal_unit_type::PPS,
        };
        assert!(pps.is_pps());
        assert!(!pps.is_slice());
    }

    #[test]
    fn test_nal_unit_to_annex_b() {
        let nal = NalUnit {
            data: vec![0x65, 0xAA, 0xBB],
            nal_type: nal_unit_type::IDR_SLICE,
        };
        let annex_b = nal.to_annex_b();
        assert_eq!(&annex_b[..4], &[0x00, 0x00, 0x00, 0x01]);
        assert_eq!(&annex_b[4..], &[0x65, 0xAA, 0xBB]);
    }

    #[test]
    fn test_parse_annex_b_keyframe() {
        // Build an Annex B stream: SPS + PPS + IDR slice
        let mut data = Vec::new();
        // SPS
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        data.extend_from_slice(&[0x67, 0x64, 0x00, 0x1f, 0xAC]);
        // PPS
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        data.extend_from_slice(&[0x68, 0xEE, 0x3C, 0x80]);
        // IDR slice
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        data.extend_from_slice(&[0x65, 0x88, 0x80, 0x40, 0x00]);

        let parsed = parse_annex_b(&data);
        assert!(parsed.is_keyframe);
        assert!(parsed.sps.is_some());
        assert!(parsed.pps.is_some());
        assert_eq!(parsed.sps.unwrap(), vec![0x67, 0x64, 0x00, 0x1f, 0xAC]);
        assert_eq!(parsed.pps.unwrap(), vec![0x68, 0xEE, 0x3C, 0x80]);
        assert_eq!(parsed.nals.len(), 1);
        assert!(parsed.nals[0].is_idr());
    }

    #[test]
    fn test_parse_annex_b_non_keyframe() {
        let mut data = Vec::new();
        // Non-IDR slice only
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        data.extend_from_slice(&[0x41, 0x9A, 0x00, 0x10]);

        let parsed = parse_annex_b(&data);
        assert!(!parsed.is_keyframe);
        assert!(parsed.sps.is_none());
        assert!(parsed.pps.is_none());
        assert_eq!(parsed.nals.len(), 1);
        assert!(!parsed.nals[0].is_idr());
    }

    #[test]
    fn test_parse_annex_b_3byte_start_codes() {
        let mut data = Vec::new();
        // 3-byte start code + non-IDR slice
        data.extend_from_slice(&[0x00, 0x00, 0x01]);
        data.extend_from_slice(&[0x41, 0x9A, 0x00]);

        let parsed = parse_annex_b(&data);
        assert_eq!(parsed.nals.len(), 1);
        assert_eq!(parsed.nals[0].nal_type, nal_unit_type::NON_IDR_SLICE);
    }

    #[test]
    fn test_parse_annex_b_empty() {
        let parsed = parse_annex_b(&[]);
        assert!(!parsed.is_keyframe);
        assert!(parsed.sps.is_none());
        assert!(parsed.pps.is_none());
        assert!(parsed.nals.is_empty());
    }

    #[test]
    fn test_default_config() {
        let config = CmafConfig::default();
        assert_eq!(config.fragment_duration_ms, 2000);
        assert_eq!(config.timescale, 90000);
    }

    #[test]
    fn test_muxer_initialization() {
        let mut muxer = CmafMuxer::new(CmafConfig::default());
        assert!(!muxer.is_initialized());

        let sps = vec![0x67, 0x64, 0x00, 0x1f, 0xac, 0xd9, 0x40, 0x50];
        let pps = vec![0x68, 0xee, 0x3c, 0x80];

        let init = muxer.create_init_segment(&sps, &pps, 1920, 1080);
        assert!(muxer.is_initialized());
        assert!(!init.is_empty());

        // Check ftyp box
        assert_eq!(&init[4..8], b"ftyp");
        // Check moov box exists
        assert!(init.windows(4).any(|w| w == b"moov"));
    }

    #[test]
    fn test_ftyp_box() {
        let muxer = CmafMuxer::new(CmafConfig::default());
        let mut buf = Vec::new();
        muxer.write_ftyp(&mut buf);

        let size = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(&buf[4..8], b"ftyp");
        assert_eq!(size as usize, buf.len());
    }

    #[test]
    fn test_add_frame_before_init() {
        let mut muxer = CmafMuxer::new(CmafConfig::default());
        let nals = vec![NalUnit {
            data: vec![0x65, 0x88],
            nal_type: nal_unit_type::IDR_SLICE,
        }];
        let result = muxer.add_frame(&nals, 0, 0, 3000, true);
        assert!(result.is_none());
    }

    #[test]
    fn test_muxer_add_frame_and_flush() {
        let mut muxer = CmafMuxer::new(CmafConfig {
            fragment_duration_ms: 33,
            timescale: 90000,
        });

        let sps = vec![0x67, 0x64, 0x00, 0x1f];
        let pps = vec![0x68, 0xee, 0x3c, 0x80];
        muxer.create_init_segment(&sps, &pps, 640, 480);

        // First frame (keyframe) - no segment returned yet
        let nals = vec![NalUnit {
            data: vec![0x65, 0x88, 0x80],
            nal_type: nal_unit_type::IDR_SLICE,
        }];
        let seg = muxer.add_frame(&nals, 0, 0, 3000, true);
        assert!(seg.is_none());
        assert_eq!(muxer.pending_frame_count(), 1);

        // Second frame (non-keyframe)
        let nals = vec![NalUnit {
            data: vec![0x41, 0x9A, 0x00],
            nal_type: nal_unit_type::NON_IDR_SLICE,
        }];
        let seg = muxer.add_frame(&nals, 3000, 3000, 3000, false);
        assert!(seg.is_none());
        assert_eq!(muxer.pending_frame_count(), 2);

        // Flush remaining
        let seg = muxer.flush();
        assert!(seg.is_some());
        let seg = seg.unwrap();
        // Check styp box
        assert!(seg.windows(4).any(|w| w == b"styp"));
        // Check moof box
        assert!(seg.windows(4).any(|w| w == b"moof"));
        // Check mdat box
        assert!(seg.windows(4).any(|w| w == b"mdat"));
    }

    // ========================================
    // AV1 tests
    // ========================================

    #[test]
    fn test_parse_av1_obus() {
        // Build a fake AV1 bitstream with two OBUs:
        // OBU 1: Sequence Header (type=1), has_size=1, 3 bytes payload
        // Header byte: obu_type=1 (bits 6-3), no extension, has_size=1
        // 0b0_0001_0_1_0 = 0x0A
        let mut data = Vec::new();
        data.push(0x0A); // Sequence Header OBU header
        data.push(3); // size = 3 (leb128)
        data.extend_from_slice(&[0xAA, 0xBB, 0xCC]); // payload
                                                     // OBU 2: Frame (type=6), has_size=1, 2 bytes payload
                                                     // 0b0_0110_0_1_0 = 0x32
        data.push(0x32); // Frame OBU header
        data.push(2); // size = 2
        data.extend_from_slice(&[0xDD, 0xEE]); // payload

        let obus = parse_av1_obus(&data);
        assert_eq!(obus.len(), 2);
        assert_eq!(obus[0].obu_type, obu_type::SEQUENCE_HEADER);
        assert_eq!(obus[0].data.len(), 5); // header(1) + size(1) + payload(3)
        assert_eq!(obus[1].obu_type, obu_type::FRAME);
        assert_eq!(obus[1].data.len(), 4); // header(1) + size(1) + payload(2)
    }

    #[test]
    fn test_extract_av1_sequence_header() {
        let mut data = Vec::new();
        // Temporal Delimiter (type=2)
        data.push(0x12); // 0b0_0010_0_1_0
        data.push(0); // size = 0
                      // Sequence Header (type=1)
        data.push(0x0A); // 0b0_0001_0_1_0
        data.push(2);
        data.extend_from_slice(&[0x11, 0x22]);

        let seq_hdr = extract_av1_sequence_header(&data);
        assert!(seq_hdr.is_some());
        let seq_hdr = seq_hdr.unwrap();
        assert_eq!(seq_hdr[0], 0x0A); // starts with OBU header
        assert_eq!(seq_hdr.len(), 4); // header + size + 2 bytes payload
    }

    #[test]
    fn test_av1_cmaf_init_segment() {
        let mut muxer = Av1CmafMuxer::new(CmafConfig::default());
        assert!(!muxer.is_initialized());

        // Fake sequence header OBU
        let seq_hdr = vec![0x0A, 0x02, 0x00, 0x00];
        let init = muxer.create_init_segment(&seq_hdr, 640, 480);
        assert!(muxer.is_initialized());
        assert!(!init.is_empty());

        // Check ftyp
        assert_eq!(&init[4..8], b"ftyp");
        // Check moov exists
        assert!(init.windows(4).any(|w| w == b"moov"));
        // Check av01 sample entry exists
        assert!(init.windows(4).any(|w| w == b"av01"));
        // Check av1C config box exists
        assert!(init.windows(4).any(|w| w == b"av1C"));
    }

    #[test]
    fn test_av1_cmaf_add_frame_and_flush() {
        let mut muxer = Av1CmafMuxer::new(CmafConfig {
            fragment_duration_ms: 33,
            timescale: 90000,
        });

        let seq_hdr = vec![0x0A, 0x02, 0x00, 0x00];
        muxer.create_init_segment(&seq_hdr, 640, 480);

        // First frame (keyframe)
        let seg = muxer.add_frame(
            &[0x32, 0x05, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE],
            0,
            0,
            3000,
            true,
        );
        assert!(seg.is_none());
        assert_eq!(muxer.pending_frame_count(), 1);

        // Second frame
        let seg = muxer.add_frame(&[0x32, 0x03, 0x11, 0x22, 0x33], 3000, 3000, 3000, false);
        assert!(seg.is_none());

        // Flush
        let seg = muxer.flush();
        assert!(seg.is_some());
        let seg = seg.unwrap();
        assert!(seg.windows(4).any(|w| w == b"styp"));
        assert!(seg.windows(4).any(|w| w == b"moof"));
        assert!(seg.windows(4).any(|w| w == b"mdat"));
    }
}

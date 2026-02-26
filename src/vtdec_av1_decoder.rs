//! VideoToolbox hardware AV1 decoder for macOS.
//!
//! Uses Apple's VTDecompressionSession (macOS 13+ / Apple Silicon).
//! Supports:
//! - 8-bit: BGRA output → RGB conversion (for color frames)
//! - 10-bit: Biplanar 10-bit output → Y-plane extraction (for depth frames)

use anyhow::Result;
use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
use core_foundation::data::CFData;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation_sys::base::OSStatus;
use core_media_sys::CMTime;
use libc::c_void;
use std::ptr;
use std::sync::{Arc, Mutex as StdMutex};
use video_toolbox_sys::cv_types::CVPixelBufferRef;
use video_toolbox_sys::decompression::{
    VTDecompressionOutputCallbackRecord, VTDecompressionSessionCreate,
    VTDecompressionSessionDecodeFrame, VTDecompressionSessionInvalidate, VTDecompressionSessionRef,
};

/// `kCMVideoCodecType_AV1` = FourCC `'av01'`
const K_CM_VIDEO_CODEC_TYPE_AV1: u32 = 0x61763031;
/// `kCVPixelFormatType_32BGRA`
const PIXEL_FMT_BGRA: i32 = 0x42475241u32 as i32;
/// `kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange` (P010-compatible)
const PIXEL_FMT_BIPLANAR_10BIT: i32 = 0x78343230u32 as i32;

// ── CoreMedia FFI ──────────────────────────────────────────────────────

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMVideoFormatDescriptionCreate(
        allocator: *const c_void,
        codec_type: u32,
        width: i32,
        height: i32,
        extensions: *const c_void,
        format_description_out: *mut *mut c_void,
    ) -> OSStatus;

    fn CMSampleBufferCreate(
        allocator: *const c_void,
        data_buffer: *const c_void,
        data_ready: bool,
        make_data_ready_callback: *const c_void,
        make_data_ready_refcon: *const c_void,
        format_description: *const c_void,
        num_samples: i64,
        num_sample_timing_entries: i64,
        sample_timing_array: *const CMSampleTimingInfo,
        num_sample_size_entries: i64,
        sample_size_array: *const usize,
        sample_buffer_out: *mut *mut c_void,
    ) -> OSStatus;

    fn CMBlockBufferCreateWithMemoryBlock(
        allocator: *const c_void,
        memory_block: *mut c_void,
        block_length: usize,
        block_allocator: *const c_void,
        custom_block_source: *const c_void,
        offset_to_data: usize,
        data_length: usize,
        flags: u32,
        block_buffer_out: *mut *mut c_void,
    ) -> OSStatus;
}

// ── CoreVideo FFI ──────────────────────────────────────────────────────

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn CVPixelBufferLockBaseAddress(pixel_buffer: CVPixelBufferRef, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(pixel_buffer: CVPixelBufferRef, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddress(pixel_buffer: CVPixelBufferRef) -> *mut c_void;
    fn CVPixelBufferGetWidth(pixel_buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetHeight(pixel_buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetBytesPerRow(pixel_buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetBaseAddressOfPlane(buf: CVPixelBufferRef, idx: usize) -> *mut c_void;
    fn CVPixelBufferGetBytesPerRowOfPlane(buf: CVPixelBufferRef, idx: usize) -> usize;
    fn CVPixelBufferGetWidthOfPlane(buf: CVPixelBufferRef, idx: usize) -> usize;
    fn CVPixelBufferGetHeightOfPlane(buf: CVPixelBufferRef, idx: usize) -> usize;
}

#[repr(C)]
#[derive(Copy, Clone)]
struct CMSampleTimingInfo {
    duration: CMTime,
    presentation_time_stamp: CMTime,
    decode_time_stamp: CMTime,
}

// ── Public types ───────────────────────────────────────────────────────

/// A decoded video frame.
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    /// RGB u8 for 8-bit, or raw Y-plane u16 LE (MSB-aligned 10-bit) for depth.
    pub data: Vec<u8>,
    pub bits_per_component: u8,
}

// ── Decoder ────────────────────────────────────────────────────────────

struct CallbackState {
    frame_slot: StdMutex<Option<DecodedFrame>>,
    high_bitdepth: bool,
}

/// VideoToolbox AV1 decoder (macOS 13+, Apple Silicon).
pub struct VtAv1Decoder {
    session: VTDecompressionSessionRef,
    format_desc: *mut c_void,
    state: Arc<CallbackState>,
    high_bitdepth: bool,
    frame_count: u64,
}

unsafe impl Send for VtAv1Decoder {}

impl VtAv1Decoder {
    pub fn new(high_bitdepth: bool) -> Result<Self> {
        Ok(Self {
            session: ptr::null_mut(),
            format_desc: ptr::null_mut(),
            state: Arc::new(CallbackState {
                frame_slot: StdMutex::new(None),
                high_bitdepth,
            }),
            high_bitdepth,
            frame_count: 0,
        })
    }

    /// Decode AV1 OBU data and return the decoded frame.
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<DecodedFrame>> {
        // Lazily create session on first OBU temporal unit containing a sequence header.
        if self.session.is_null() {
            let seq = match parse_sequence_header_from_obus(data) {
                Some(s) => s,
                None => return Ok(None),
            };
            self.create_session(&seq, data)?;
        }

        // Clear slot
        if let Ok(mut slot) = self.state.frame_slot.lock() {
            *slot = None;
        }

        unsafe {
            let mut buf: Box<Vec<u8>> = Box::new(data.to_vec());

            // Block buffer
            let mut block_buf: *mut c_void = ptr::null_mut();
            let st = CMBlockBufferCreateWithMemoryBlock(
                ptr::null(),
                buf.as_mut_ptr() as *mut c_void,
                buf.len(),
                ptr::null(),
                ptr::null(),
                0,
                buf.len(),
                0,
                &mut block_buf,
            );
            if st != 0 {
                anyhow::bail!("[vtdec-av1] CMBlockBufferCreate failed: {st}");
            }

            // Sample buffer
            let timing = CMSampleTimingInfo {
                duration: CMTime {
                    value: 1,
                    timescale: 30,
                    flags: 1,
                    epoch: 0,
                },
                presentation_time_stamp: CMTime {
                    value: self.frame_count as i64,
                    timescale: 30,
                    flags: 1,
                    epoch: 0,
                },
                decode_time_stamp: CMTime {
                    value: self.frame_count as i64,
                    timescale: 30,
                    flags: 1,
                    epoch: 0,
                },
            };
            let sample_size = buf.len();
            let mut sample_buf: *mut c_void = ptr::null_mut();

            let st = CMSampleBufferCreate(
                ptr::null(),
                block_buf,
                true,
                ptr::null(),
                ptr::null(),
                self.format_desc,
                1,
                1,
                &timing,
                1,
                &sample_size,
                &mut sample_buf,
            );
            if st != 0 {
                CFRelease(block_buf as CFTypeRef);
                anyhow::bail!("[vtdec-av1] CMSampleBufferCreate failed: {st}");
            }

            // Synchronous decode
            let mut info_flags: u32 = 0;
            let st = VTDecompressionSessionDecodeFrame(
                self.session,
                sample_buf as *mut _,
                0,
                ptr::null_mut(),
                &mut info_flags,
            );
            CFRelease(sample_buf as CFTypeRef);

            if st != 0 {
                anyhow::bail!("[vtdec-av1] decode failed: status={st}, flags=0x{info_flags:x}");
            }
        }

        self.frame_count += 1;

        Ok(self.state.frame_slot.lock().ok().and_then(|mut s| s.take()))
    }

    fn create_session(&mut self, seq: &Av1SeqHdr, obu_data: &[u8]) -> Result<()> {
        self.destroy_session();

        let pixel_fmt = if self.high_bitdepth {
            PIXEL_FMT_BIPLANAR_10BIT
        } else {
            PIXEL_FMT_BGRA
        };

        unsafe {
            // Build av1C extension for the format description
            let av1c_bytes = build_av1c(seq, obu_data);
            let av1c_data = CFData::from_buffer(&av1c_bytes);
            let av1c_key = CFString::new("av1C");
            let inner =
                CFDictionary::from_CFType_pairs(&[(av1c_key.as_CFType(), av1c_data.as_CFType())]);
            let atoms_key = CFString::new("SampleDescriptionExtensionAtoms");
            let extensions =
                CFDictionary::from_CFType_pairs(&[(atoms_key.as_CFType(), inner.as_CFType())]);

            // Format description
            let mut fmt_desc: *mut c_void = ptr::null_mut();
            let st = CMVideoFormatDescriptionCreate(
                ptr::null(),
                K_CM_VIDEO_CODEC_TYPE_AV1,
                seq.width as i32,
                seq.height as i32,
                extensions.as_concrete_TypeRef() as *const _,
                &mut fmt_desc,
            );
            if st != 0 {
                anyhow::bail!("[vtdec-av1] CMVideoFormatDescriptionCreate failed: {st}");
            }
            self.format_desc = fmt_desc;

            // Destination pixel buffer attributes
            let pf_key = CFString::new("PixelFormatType");
            let pf_val = CFNumber::from(pixel_fmt);
            let ios_key = CFString::new("IOSurfaceProperties");
            let empty: Vec<(CFString, CFString)> = Vec::new();
            let ios_val = CFDictionary::from_CFType_pairs(&empty);
            let dest = CFDictionary::from_CFType_pairs(&[
                (pf_key.as_CFType(), pf_val.as_CFType()),
                (ios_key.as_CFType(), ios_val.as_CFType()),
            ]);

            // Callback
            let state_ptr = Arc::into_raw(self.state.clone()) as *mut c_void;
            let cb = VTDecompressionOutputCallbackRecord {
                decompressionOutputCallback: vt_av1_callback,
                decompressionOutputRefCon: state_ptr,
            };

            let mut session: VTDecompressionSessionRef = ptr::null_mut();
            let st = VTDecompressionSessionCreate(
                ptr::null(),
                fmt_desc as *mut _,
                ptr::null(),
                dest.as_concrete_TypeRef() as *const _,
                &cb,
                &mut session,
            );
            // Reconstruct Arc to avoid leak (callback uses raw pointer directly)
            let _ = Arc::from_raw(state_ptr as *const CallbackState);

            if st != 0 {
                CFRelease(fmt_desc as CFTypeRef);
                self.format_desc = ptr::null_mut();
                anyhow::bail!("[vtdec-av1] VTDecompressionSessionCreate failed: {st}");
            }
            self.session = session;
        }

        tracing::info!(
            "[vtdec-av1] session: {}x{}, {}bit, pixel_fmt=0x{:08x}",
            seq.width,
            seq.height,
            seq.bit_depth,
            pixel_fmt as u32,
        );
        Ok(())
    }

    fn destroy_session(&mut self) {
        unsafe {
            if !self.session.is_null() {
                VTDecompressionSessionInvalidate(self.session);
                self.session = ptr::null_mut();
            }
            if !self.format_desc.is_null() {
                CFRelease(self.format_desc as CFTypeRef);
                self.format_desc = ptr::null_mut();
            }
        }
    }
}

impl Drop for VtAv1Decoder {
    fn drop(&mut self) {
        self.destroy_session();
    }
}

// ── VT decompression callback ──────────────────────────────────────────

extern "C" fn vt_av1_callback(
    ref_con: *mut c_void,
    _source: *mut c_void,
    status: OSStatus,
    _flags: u32,
    image_buffer: CVPixelBufferRef,
    _pts: CMTime,
    _dur: CMTime,
) {
    if status != 0 {
        tracing::error!("[vtdec-av1] callback status: {status}");
        return;
    }
    if image_buffer.is_null() {
        return;
    }

    let state = unsafe { &*(ref_con as *const CallbackState) };

    unsafe {
        CVPixelBufferLockBaseAddress(image_buffer, 0);

        let frame = if state.high_bitdepth {
            extract_y_plane_10bit(image_buffer)
        } else {
            extract_bgra_to_rgb(image_buffer)
        };

        if let Some(f) = frame {
            if let Ok(mut slot) = state.frame_slot.lock() {
                *slot = Some(f);
            }
        }

        CVPixelBufferUnlockBaseAddress(image_buffer, 0);
    }
}

unsafe fn extract_bgra_to_rgb(buf: CVPixelBufferRef) -> Option<DecodedFrame> {
    let base = CVPixelBufferGetBaseAddress(buf);
    if base.is_null() {
        return None;
    }
    let w = CVPixelBufferGetWidth(buf);
    let h = CVPixelBufferGetHeight(buf);
    let stride = CVPixelBufferGetBytesPerRow(buf);
    let src = std::slice::from_raw_parts(base as *const u8, stride * h);

    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        let row = &src[y * stride..y * stride + w * 4];
        for px in row.chunks_exact(4) {
            rgb.push(px[2]); // R
            rgb.push(px[1]); // G
            rgb.push(px[0]); // B
        }
    }
    Some(DecodedFrame {
        width: w as u32,
        height: h as u32,
        data: rgb,
        bits_per_component: 8,
    })
}

unsafe fn extract_y_plane_10bit(buf: CVPixelBufferRef) -> Option<DecodedFrame> {
    let y_base = CVPixelBufferGetBaseAddressOfPlane(buf, 0);
    if y_base.is_null() {
        return None;
    }
    let w = CVPixelBufferGetWidthOfPlane(buf, 0);
    let h = CVPixelBufferGetHeightOfPlane(buf, 0);
    let stride = CVPixelBufferGetBytesPerRowOfPlane(buf, 0);
    let src = std::slice::from_raw_parts(y_base as *const u8, stride * h);

    // Each pixel is u16 LE, 10-bit MSB-aligned (P010 compatible)
    let mut out = Vec::with_capacity(w * h * 2);
    for row in 0..h {
        let start = row * stride;
        let end = start + w * 2;
        if end <= src.len() {
            out.extend_from_slice(&src[start..end]);
        }
    }
    Some(DecodedFrame {
        width: w as u32,
        height: h as u32,
        data: out,
        bits_per_component: 10,
    })
}

// ── AV1 OBU parsing ───────────────────────────────────────────────────

struct Av1SeqHdr {
    seq_profile: u32,
    seq_level_idx_0: u32,
    seq_tier_0: u32,
    width: u32,
    height: u32,
    bit_depth: u32,
    high_bitdepth: bool,
    twelve_bit: bool,
    mono_chrome: bool,
    sub_x: u32,
    sub_y: u32,
    chroma_sample_position: u32,
}

fn parse_sequence_header_from_obus(data: &[u8]) -> Option<Av1SeqHdr> {
    let mut pos = 0;
    while pos < data.len() {
        let hdr = data[pos];
        let obu_type = (hdr >> 3) & 0xF;
        let has_ext = (hdr >> 2) & 1 != 0;
        let has_size = (hdr >> 1) & 1 != 0;
        pos += 1;
        if has_ext {
            if pos >= data.len() {
                return None;
            }
            pos += 1;
        }
        let obu_size = if has_size {
            let (sz, n) = read_leb128(&data[pos..])?;
            pos += n;
            sz as usize
        } else {
            data.len() - pos
        };
        if obu_type == 1 {
            let end = (pos + obu_size).min(data.len());
            return parse_seq_hdr(&data[pos..end]);
        }
        pos += obu_size;
    }
    None
}

fn read_leb128(data: &[u8]) -> Option<(u64, usize)> {
    let mut val = 0u64;
    for i in 0..8 {
        if i >= data.len() {
            return None;
        }
        val |= ((data[i] & 0x7F) as u64) << (i * 7);
        if data[i] & 0x80 == 0 {
            return Some((val, i + 1));
        }
    }
    Some((val, 8))
}

/// Extract the raw sequence header OBU (header + size + payload) for av1C.
fn extract_seq_hdr_obu(data: &[u8]) -> Option<Vec<u8>> {
    let mut pos = 0;
    while pos < data.len() {
        let start = pos;
        let hdr = data[pos];
        let obu_type = (hdr >> 3) & 0xF;
        let has_ext = (hdr >> 2) & 1 != 0;
        let has_size = (hdr >> 1) & 1 != 0;
        pos += 1;
        if has_ext {
            pos += 1;
        }
        let obu_size = if has_size {
            let (sz, n) = read_leb128(&data[pos..])?;
            pos += n;
            sz as usize
        } else {
            data.len() - pos
        };
        let end = pos + obu_size;
        if obu_type == 1 {
            return Some(data[start..end.min(data.len())].to_vec());
        }
        pos = end;
    }
    None
}

/// Build the 4-byte av1C header + configOBUs.
fn build_av1c(seq: &Av1SeqHdr, obu_data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    // Byte 0: marker=1, version=1 → 0x81
    out.push(0x81);
    // Byte 1: seq_profile(3) | seq_level_idx_0(5)
    out.push(((seq.seq_profile & 7) << 5 | (seq.seq_level_idx_0 & 0x1F)) as u8);
    // Byte 2: tier(1)|hbd(1)|12b(1)|mono(1)|subx(1)|suby(1)|csp(2)
    out.push(
        ((seq.seq_tier_0 & 1) << 7
            | (seq.high_bitdepth as u32 & 1) << 6
            | (seq.twelve_bit as u32 & 1) << 5
            | (seq.mono_chrome as u32 & 1) << 4
            | (seq.sub_x & 1) << 3
            | (seq.sub_y & 1) << 2
            | (seq.chroma_sample_position & 3)) as u8,
    );
    // Byte 3: reserved(3)=0 | initial_delay_present=0 | reserved(4)=0
    out.push(0x00);
    // configOBUs: raw sequence header OBU
    if let Some(raw) = extract_seq_hdr_obu(obu_data) {
        out.extend_from_slice(&raw);
    }
    out
}

// ── Bit reader & sequence header parser (AV1 spec §5.5) ───────────────

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn f(&mut self, n: u32) -> Option<u32> {
        if n == 0 {
            return Some(0);
        }
        let mut v = 0u32;
        for _ in 0..n {
            let byte = self.pos / 8;
            let bit = 7 - (self.pos % 8);
            if byte >= self.data.len() {
                return None;
            }
            v = (v << 1) | ((self.data[byte] >> bit) as u32 & 1);
            self.pos += 1;
        }
        Some(v)
    }
    fn uvlc(&mut self) -> Option<u32> {
        let mut lz = 0u32;
        while self.f(1)? == 0 {
            lz += 1;
            if lz > 32 {
                return None;
            }
        }
        if lz == 0 {
            return Some(0);
        }
        Some((1 << lz) - 1 + self.f(lz)?)
    }
}

fn parse_seq_hdr(payload: &[u8]) -> Option<Av1SeqHdr> {
    let mut r = Bits::new(payload);

    let seq_profile = r.f(3)?;
    let _still = r.f(1)?;
    let reduced = r.f(1)? != 0;

    let mut lvl0 = 0u32;
    let mut tier0 = 0u32;
    let mut dm_present = false;
    let mut buf_delay_len = 0u32;

    if reduced {
        lvl0 = r.f(5)?;
    } else {
        let timing = r.f(1)? != 0;
        if timing {
            r.f(32)?; // num_units_in_display_tick
            r.f(32)?; // time_scale
            if r.f(1)? != 0 {
                r.uvlc()?;
            } // equal_picture_interval
            dm_present = r.f(1)? != 0;
            if dm_present {
                buf_delay_len = r.f(5)?;
                r.f(32)?; // num_units_in_decoding_tick
                r.f(5)?; // buffer_removal_time_length_minus_1
                r.f(5)?; // frame_presentation_time_length_minus_1
            }
        }
        let init_display = r.f(1)? != 0;
        let op_cnt = r.f(5)?;
        for i in 0..=op_cnt {
            r.f(12)?; // operating_point_idc
            let li = r.f(5)?;
            if i == 0 {
                lvl0 = li;
            }
            let ti = if li > 7 { r.f(1)? } else { 0 };
            if i == 0 {
                tier0 = ti;
            }
            if dm_present {
                if r.f(1)? != 0 {
                    let n = buf_delay_len + 1;
                    r.f(n)?;
                    r.f(n)?;
                    r.f(1)?;
                }
            }
            if init_display {
                if r.f(1)? != 0 {
                    r.f(4)?;
                }
            }
        }
    }

    // Frame dimensions
    let wb = r.f(4)? + 1;
    let hb = r.f(4)? + 1;
    let width = r.f(wb)? + 1;
    let height = r.f(hb)? + 1;

    if !reduced {
        if r.f(1)? != 0 {
            // frame_id_numbers_present
            r.f(4)?;
            r.f(3)?;
        }
    }

    r.f(1)?; // use_128x128_superblock
    r.f(1)?; // enable_filter_intra
    r.f(1)?; // enable_intra_edge_filter

    if !reduced {
        r.f(1)?; // enable_interintra_compound
        r.f(1)?; // enable_masked_compound
        r.f(1)?; // enable_warped_motion
        r.f(1)?; // enable_dual_filter
        let order_hint = r.f(1)? != 0;
        if order_hint {
            r.f(1)?; // enable_jnt_comp
            r.f(1)?; // enable_ref_frame_mvs
        }
        let scsct = if r.f(1)? != 0 { 2u32 } else { r.f(1)? };
        if scsct > 0 {
            if r.f(1)? == 0 {
                r.f(1)?;
            }
        }
        if order_hint {
            r.f(3)?; // order_hint_bits_minus_1
        }
    }

    r.f(1)?; // enable_superres
    r.f(1)?; // enable_cdef
    r.f(1)?; // enable_restoration

    // ── color_config() ─────────────────────────────────────────────────
    let high_bitdepth = r.f(1)? != 0;
    let twelve_bit = if seq_profile == 2 && high_bitdepth {
        r.f(1)? != 0
    } else {
        false
    };
    let bit_depth: u32 = match (high_bitdepth, twelve_bit) {
        (true, true) => 12,
        (true, false) => 10,
        _ => 8,
    };

    let mono_chrome = if seq_profile != 1 {
        r.f(1)? != 0
    } else {
        false
    };

    let color_desc = r.f(1)? != 0;
    let (cp, tc, mc) = if color_desc {
        (r.f(8)?, r.f(8)?, r.f(8)?)
    } else {
        (2, 2, 2)
    };

    let (sub_x, sub_y, csp);
    if mono_chrome {
        r.f(1)?; // color_range
        sub_x = 1;
        sub_y = 1;
        csp = 0;
    } else if cp == 1 && tc == 13 && mc == 0 {
        // sRGB / BT.709
        sub_x = 0;
        sub_y = 0;
        csp = 0;
    } else {
        r.f(1)?; // color_range
        if seq_profile == 0 {
            sub_x = 1;
            sub_y = 1;
        } else if seq_profile == 1 {
            sub_x = 0;
            sub_y = 0;
        } else if bit_depth == 12 {
            sub_x = r.f(1)?;
            sub_y = if sub_x != 0 { r.f(1)? } else { 0 };
        } else {
            sub_x = 1;
            sub_y = 0;
        }
        csp = if sub_x != 0 && sub_y != 0 { r.f(2)? } else { 0 };
    }

    Some(Av1SeqHdr {
        seq_profile,
        seq_level_idx_0: lvl0,
        seq_tier_0: tier0,
        width,
        height,
        bit_depth,
        high_bitdepth,
        twelve_bit,
        mono_chrome,
        sub_x,
        sub_y,
        chroma_sample_position: csp,
    })
}

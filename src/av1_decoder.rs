//! Pluggable AV1 decoder — dispatches to whichever backend is feature-enabled.
//!
//! Backends: nvdec (Linux/NVIDIA), videotoolbox (macOS), dav1d (software fallback).

use anyhow::Result;

/// A decoded video frame.
pub struct DecodedFrame {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Raw pixel data (RGB u8 for 8-bit color, or raw Y-plane u16 LE for 10-bit depth).
    pub data: Vec<u8>,
    /// Bits per component (8 or 10).
    pub bits_per_component: u8,
}

/// AV1 decoder with compile-time backend selection.
pub enum Av1Decoder {
    #[cfg(feature = "nvenc")]
    Nvdec(crate::nvdec_av1_decoder::NvdecAv1Decoder),
    #[cfg(feature = "videotoolbox")]
    Vt(crate::vtdec_av1_decoder::VtAv1Decoder),
    #[cfg(feature = "dav1d")]
    Dav1d(crate::dav1d_decoder::Dav1dDecoder),
}

impl Av1Decoder {
    /// Create a new AV1 decoder.
    ///
    /// `high_bitdepth` controls the output format:
    /// - false: 8-bit RGB (for color frames)
    /// - true: 10-bit Y-plane (for depth frames)
    pub fn new(high_bitdepth: bool) -> Result<Self> {
        #[cfg(feature = "nvenc")]
        {
            return Ok(Av1Decoder::Nvdec(
                crate::nvdec_av1_decoder::NvdecAv1Decoder::new(high_bitdepth)?,
            ));
        }
        #[cfg(feature = "videotoolbox")]
        {
            return Ok(Av1Decoder::Vt(crate::vtdec_av1_decoder::VtAv1Decoder::new(
                high_bitdepth,
            )?));
        }
        #[cfg(all(
            feature = "dav1d",
            not(feature = "nvenc"),
            not(feature = "videotoolbox")
        ))]
        {
            let _ = high_bitdepth; // dav1d auto-detects bit depth
            return Ok(Av1Decoder::Dav1d(crate::dav1d_decoder::Dav1dDecoder::new()?));
        }
    }

    /// Decode AV1 OBU data and return the decoded frame.
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<DecodedFrame>> {
        let frame = match self {
            #[cfg(feature = "nvenc")]
            Av1Decoder::Nvdec(dec) => dec.decode(data)?,
            #[cfg(feature = "videotoolbox")]
            Av1Decoder::Vt(dec) => dec.decode(data)?,
            #[cfg(feature = "dav1d")]
            Av1Decoder::Dav1d(dec) => dec.decode(data)?,
        };
        Ok(frame.map(|f| DecodedFrame {
            width: f.width,
            height: f.height,
            data: f.data,
            bits_per_component: f.bits_per_component,
        }))
    }
}

/// Convert P010/P016 Y-plane u16 values to depth in millimeters.
///
/// Both nvdec (P016) and dav1d (P010) store 10-bit values MSB-aligned in u16:
/// `val = gray10 << 6`. The server encodes depth as:
/// `gray10 = min(depth_mm >> depth_shift, 1023)`.
/// So: `depth_mm = (val >> 6) << depth_shift`.
pub fn p010_y_to_depth_mm(y_data: &[u8], depth_shift: u32) -> Vec<u16> {
    let pixel_count = y_data.len() / 2;
    let mut depth = Vec::with_capacity(pixel_count);
    for i in 0..pixel_count {
        let val = u16::from_le_bytes([y_data[i * 2], y_data[i * 2 + 1]]);
        let gray10 = val >> 6;
        let mm = (gray10 as u32) << depth_shift;
        depth.push(mm as u16);
    }
    depth
}

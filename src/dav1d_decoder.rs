//! AV1 decoder using dav1d.
//!
//! Wraps the `dav1d` crate to decode AV1 OBUs into raw pixel data.
//! Supports:
//! - 8-bit YUV (I420) → RGB conversion for color frames
//! - 10-bit Y-plane extraction for depth frames (P010 encoding)

use anyhow::Result;
use dav1d::PlanarImageComponent;

/// A decoded video frame.
pub struct DecodedFrame {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Raw pixel data (RGB u8 for color, or raw Y-plane bytes for depth).
    pub data: Vec<u8>,
    /// Bits per component (8 or 10).
    pub bits_per_component: u8,
}

/// AV1 decoder backed by dav1d.
pub struct Dav1dDecoder {
    decoder: dav1d::Decoder,
}

impl Dav1dDecoder {
    /// Create a new decoder with default settings.
    pub fn new() -> Result<Self> {
        let decoder = dav1d::Decoder::new()
            .map_err(|e| anyhow::anyhow!("Failed to create dav1d decoder: {:?}", e))?;
        Ok(Self { decoder })
    }

    /// Decode AV1 OBU data and return the decoded frame.
    ///
    /// The input should be raw AV1 OBUs (temporal unit).
    /// Returns None if the decoder needs more data.
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<DecodedFrame>> {
        // Send data to decoder
        match self.decoder.send_data(
            data.to_vec(),
            None, // offset
            None, // timestamp
            None, // duration
        ) {
            Ok(()) => {}
            Err(dav1d::Error::Again) => {
                // Decoder is full, try to drain a picture first
            }
            Err(e) => return Err(anyhow::anyhow!("dav1d send_data error: {:?}", e)),
        }

        // Try to get decoded picture
        match self.decoder.get_picture() {
            Ok(picture) => {
                let width = picture.width();
                let height = picture.height();
                let bit_depth = picture.bit_depth();

                let bpc = if bit_depth > 8 { 10u8 } else { 8u8 };

                let data = if bpc == 10 {
                    // 10-bit: extract raw Y-plane as bytes (u16 LE)
                    extract_y_plane_10bit(&picture)
                } else {
                    // 8-bit: convert YUV I420 to RGB
                    yuv420_to_rgb(&picture)
                };

                Ok(Some(DecodedFrame {
                    width,
                    height,
                    data,
                    bits_per_component: bpc,
                }))
            }
            Err(dav1d::Error::Again) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("dav1d get_picture error: {:?}", e)),
        }
    }
}

/// Extract the Y plane from a 10-bit picture as P010-formatted bytes.
///
/// dav1d outputs 10-bit values as native u16 (right-aligned, range 0-1023).
/// Downstream code (`p010_y_to_depth_mm`) expects P010 format where 10-bit
/// values are MSB-aligned in u16 (val << 6). We left-shift here so the
/// output matches P010 convention used by nvdec/videotoolbox backends.
fn extract_y_plane_10bit(picture: &dav1d::Picture) -> Vec<u8> {
    let width = picture.width() as usize;
    let height = picture.height() as usize;
    let y_plane = picture.plane(PlanarImageComponent::Y);
    let stride = picture.stride(PlanarImageComponent::Y) as usize;

    // Output: width * height * 2 bytes (u16 per pixel, P010 MSB-aligned)
    let mut out = Vec::with_capacity(width * height * 2);
    for row in 0..height {
        let row_start = row * stride;
        for col in 0..width {
            let byte_offset = row_start + col * 2;
            if byte_offset + 1 < y_plane.len() {
                let native = u16::from_le_bytes([y_plane[byte_offset], y_plane[byte_offset + 1]]);
                let p010 = (native << 6).to_le_bytes(); // MSB-align to match P010 convention
                out.push(p010[0]);
                out.push(p010[1]);
            }
        }
    }
    out
}

/// Convert an 8-bit YUV I420 picture to RGB.
fn yuv420_to_rgb(picture: &dav1d::Picture) -> Vec<u8> {
    let width = picture.width() as usize;
    let height = picture.height() as usize;
    let y_plane = picture.plane(PlanarImageComponent::Y);
    let u_plane = picture.plane(PlanarImageComponent::U);
    let v_plane = picture.plane(PlanarImageComponent::V);
    let y_stride = picture.stride(PlanarImageComponent::Y) as usize;
    let u_stride = picture.stride(PlanarImageComponent::U) as usize;
    let v_stride = picture.stride(PlanarImageComponent::V) as usize;

    let mut rgb = vec![0u8; width * height * 3];

    for row in 0..height {
        for col in 0..width {
            let y_idx = row * y_stride + col;
            let uv_row = row / 2;
            let uv_col = col / 2;
            let u_idx = uv_row * u_stride + uv_col;
            let v_idx = uv_row * v_stride + uv_col;

            let y = y_plane.get(y_idx).copied().unwrap_or(0) as f32;
            let u = u_plane.get(u_idx).copied().unwrap_or(128) as f32;
            let v = v_plane.get(v_idx).copied().unwrap_or(128) as f32;

            // BT.601 YUV to RGB
            let r = (y + 1.402 * (v - 128.0)).clamp(0.0, 255.0) as u8;
            let g = (y - 0.344136 * (u - 128.0) - 0.714136 * (v - 128.0)).clamp(0.0, 255.0) as u8;
            let b = (y + 1.772 * (u - 128.0)).clamp(0.0, 255.0) as u8;

            let out_idx = (row * width + col) * 3;
            rgb[out_idx] = r;
            rgb[out_idx + 1] = g;
            rgb[out_idx + 2] = b;
        }
    }

    rgb
}

/// Convert P010 Y-plane u16 values to depth in millimeters.
///
/// P010 stores 10-bit values MSB-aligned in u16: `val = gray10 << 6`.
/// The server encodes depth as: `gray10 = min(depth_mm >> depth_shift, 1023)`.
/// So: `depth_mm = (val >> 6) << depth_shift`.
pub fn p010_y_to_depth_mm(y_data: &[u8], depth_shift: u32) -> Vec<u16> {
    // y_data is u16 LE bytes, 2 per pixel
    let pixel_count = y_data.len() / 2;
    let mut depth = Vec::with_capacity(pixel_count);
    for i in 0..pixel_count {
        let val = u16::from_le_bytes([y_data[i * 2], y_data[i * 2 + 1]]);
        let gray10 = val >> 6; // undo MSB alignment
        let mm = (gray10 as u32) << depth_shift;
        depth.push(mm as u16);
    }
    depth
}

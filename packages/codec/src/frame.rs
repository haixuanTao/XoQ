//! Video frame types and conversions.

use crate::{CodecError, PixelFormat, VideoFrameData};

/// A video frame with pixel data.
#[derive(Debug, Clone)]
pub struct VideoFrame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Pixel format of the frame data.
    pub pixel_format: PixelFormat,
    /// Raw pixel data.
    pub data: Vec<u8>,
    /// Frame timestamp in microseconds.
    pub timestamp_us: u64,
}

impl VideoFrame {
    /// Create a new video frame.
    #[must_use]
    pub fn new(
        width: u32,
        height: u32,
        pixel_format: PixelFormat,
        data: Vec<u8>,
        timestamp_us: u64,
    ) -> Self {
        Self {
            width,
            height,
            pixel_format,
            data,
            timestamp_us,
        }
    }

    /// Create a frame from RGB data.
    #[must_use]
    pub fn from_rgb(width: u32, height: u32, data: Vec<u8>, timestamp_us: u64) -> Self {
        Self::new(width, height, PixelFormat::Rgb, data, timestamp_us)
    }

    /// Create a frame from RGBA data.
    #[must_use]
    pub fn from_rgba(width: u32, height: u32, data: Vec<u8>, timestamp_us: u64) -> Self {
        Self::new(width, height, PixelFormat::Rgba, data, timestamp_us)
    }

    /// Create a frame from BGR data.
    #[must_use]
    pub fn from_bgr(width: u32, height: u32, data: Vec<u8>, timestamp_us: u64) -> Self {
        Self::new(width, height, PixelFormat::Bgr, data, timestamp_us)
    }

    /// Create a frame from BGRA data.
    #[must_use]
    pub fn from_bgra(width: u32, height: u32, data: Vec<u8>, timestamp_us: u64) -> Self {
        Self::new(width, height, PixelFormat::Bgra, data, timestamp_us)
    }

    /// Create a frame from NV12 data.
    #[must_use]
    pub fn from_nv12(width: u32, height: u32, data: Vec<u8>, timestamp_us: u64) -> Self {
        Self::new(width, height, PixelFormat::Nv12, data, timestamp_us)
    }

    /// Expected data size for the current dimensions and pixel format.
    #[must_use]
    pub fn expected_data_size(&self) -> usize {
        expected_frame_size(self.width, self.height, self.pixel_format)
    }

    /// Convert this frame to NV12 format.
    ///
    /// If the frame is already NV12, returns a clone.
    pub fn to_nv12(&self) -> Result<VideoFrame, CodecError> {
        match self.pixel_format {
            PixelFormat::Nv12 => Ok(self.clone()),
            PixelFormat::Rgb => {
                let nv12_data = rgb_to_nv12(&self.data, self.width, self.height)?;
                Ok(VideoFrame::new(
                    self.width,
                    self.height,
                    PixelFormat::Nv12,
                    nv12_data,
                    self.timestamp_us,
                ))
            }
            PixelFormat::Rgba => {
                let nv12_data = rgba_to_nv12(&self.data, self.width, self.height)?;
                Ok(VideoFrame::new(
                    self.width,
                    self.height,
                    PixelFormat::Nv12,
                    nv12_data,
                    self.timestamp_us,
                ))
            }
            PixelFormat::Bgr => {
                let nv12_data = bgr_to_nv12(&self.data, self.width, self.height)?;
                Ok(VideoFrame::new(
                    self.width,
                    self.height,
                    PixelFormat::Nv12,
                    nv12_data,
                    self.timestamp_us,
                ))
            }
            PixelFormat::Bgra => {
                let nv12_data = bgra_to_nv12(&self.data, self.width, self.height)?;
                Ok(VideoFrame::new(
                    self.width,
                    self.height,
                    PixelFormat::Nv12,
                    nv12_data,
                    self.timestamp_us,
                ))
            }
            PixelFormat::Argb => {
                let nv12_data = argb_to_nv12(&self.data, self.width, self.height)?;
                Ok(VideoFrame::new(
                    self.width,
                    self.height,
                    PixelFormat::Nv12,
                    nv12_data,
                    self.timestamp_us,
                ))
            }
            PixelFormat::Abgr => {
                let nv12_data = abgr_to_nv12(&self.data, self.width, self.height)?;
                Ok(VideoFrame::new(
                    self.width,
                    self.height,
                    PixelFormat::Nv12,
                    nv12_data,
                    self.timestamp_us,
                ))
            }
            PixelFormat::I420 => {
                let nv12_data = i420_to_nv12(&self.data, self.width, self.height)?;
                Ok(VideoFrame::new(
                    self.width,
                    self.height,
                    PixelFormat::Nv12,
                    nv12_data,
                    self.timestamp_us,
                ))
            }
        }
    }

    /// Convert this frame to ARGB format.
    pub fn to_argb(&self) -> Result<VideoFrame, CodecError> {
        match self.pixel_format {
            PixelFormat::Argb => Ok(self.clone()),
            PixelFormat::Rgb => {
                let argb_data = rgb_to_argb(&self.data, self.width, self.height)?;
                Ok(VideoFrame::new(
                    self.width,
                    self.height,
                    PixelFormat::Argb,
                    argb_data,
                    self.timestamp_us,
                ))
            }
            PixelFormat::Rgba => {
                let argb_data = rgba_to_argb(&self.data, self.width, self.height)?;
                Ok(VideoFrame::new(
                    self.width,
                    self.height,
                    PixelFormat::Argb,
                    argb_data,
                    self.timestamp_us,
                ))
            }
            PixelFormat::Bgr => {
                let argb_data = bgr_to_argb(&self.data, self.width, self.height)?;
                Ok(VideoFrame::new(
                    self.width,
                    self.height,
                    PixelFormat::Argb,
                    argb_data,
                    self.timestamp_us,
                ))
            }
            PixelFormat::Bgra => {
                let argb_data = bgra_to_argb(&self.data, self.width, self.height)?;
                Ok(VideoFrame::new(
                    self.width,
                    self.height,
                    PixelFormat::Argb,
                    argb_data,
                    self.timestamp_us,
                ))
            }
            _ => Err(CodecError::ConversionError(format!(
                "conversion from {:?} to ARGB not implemented",
                self.pixel_format
            ))),
        }
    }
}

impl VideoFrameData for VideoFrame {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn pixel_format(&self) -> PixelFormat {
        self.pixel_format
    }

    fn data(&self) -> &[u8] {
        &self.data
    }

    fn timestamp_us(&self) -> u64 {
        self.timestamp_us
    }
}

/// Calculate expected frame size for given dimensions and format.
#[must_use]
pub fn expected_frame_size(width: u32, height: u32, format: PixelFormat) -> usize {
    let pixels = (width * height) as usize;
    match format {
        PixelFormat::Rgb | PixelFormat::Bgr => pixels * 3,
        PixelFormat::Rgba | PixelFormat::Bgra | PixelFormat::Argb | PixelFormat::Abgr => pixels * 4,
        PixelFormat::Nv12 | PixelFormat::I420 => pixels + pixels / 2, // Y + UV (4:2:0)
    }
}

// ============================================================================
// Color conversion functions
// ============================================================================

/// Convert RGB to NV12.
fn rgb_to_nv12(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>, CodecError> {
    let w = width as usize;
    let h = height as usize;
    let expected = w * h * 3;
    if rgb.len() != expected {
        return Err(CodecError::ConversionError(format!(
            "RGB data size mismatch: expected {}, got {}",
            expected,
            rgb.len()
        )));
    }

    let y_size = w * h;
    let uv_size = y_size / 2;
    let mut nv12 = vec![0u8; y_size + uv_size];

    // Convert RGB to Y plane
    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) * 3;
            let r = rgb[idx] as i32;
            let g = rgb[idx + 1] as i32;
            let b = rgb[idx + 2] as i32;

            // BT.601 conversion
            let y_val = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
            nv12[y * w + x] = y_val.clamp(0, 255) as u8;
        }
    }

    // Convert RGB to UV plane (subsampled 2x2)
    let uv_offset = y_size;
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            // Average 2x2 block
            let mut r_sum = 0i32;
            let mut g_sum = 0i32;
            let mut b_sum = 0i32;

            for dy in 0..2 {
                for dx in 0..2 {
                    let py = (y + dy).min(h - 1);
                    let px = (x + dx).min(w - 1);
                    let idx = (py * w + px) * 3;
                    r_sum += rgb[idx] as i32;
                    g_sum += rgb[idx + 1] as i32;
                    b_sum += rgb[idx + 2] as i32;
                }
            }

            let r = r_sum / 4;
            let g = g_sum / 4;
            let b = b_sum / 4;

            // BT.601 conversion
            let u = ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
            let v = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;

            let uv_idx = uv_offset + (y / 2) * w + (x / 2) * 2;
            nv12[uv_idx] = u.clamp(0, 255) as u8;
            nv12[uv_idx + 1] = v.clamp(0, 255) as u8;
        }
    }

    Ok(nv12)
}

/// Convert RGBA to NV12.
fn rgba_to_nv12(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, CodecError> {
    let w = width as usize;
    let h = height as usize;
    let expected = w * h * 4;
    if rgba.len() != expected {
        return Err(CodecError::ConversionError(format!(
            "RGBA data size mismatch: expected {}, got {}",
            expected,
            rgba.len()
        )));
    }

    // Convert RGBA to RGB first, then to NV12
    let mut rgb = vec![0u8; w * h * 3];
    for i in 0..(w * h) {
        rgb[i * 3] = rgba[i * 4];
        rgb[i * 3 + 1] = rgba[i * 4 + 1];
        rgb[i * 3 + 2] = rgba[i * 4 + 2];
    }

    rgb_to_nv12(&rgb, width, height)
}

/// Convert BGR to NV12.
fn bgr_to_nv12(bgr: &[u8], width: u32, height: u32) -> Result<Vec<u8>, CodecError> {
    let w = width as usize;
    let h = height as usize;
    let expected = w * h * 3;
    if bgr.len() != expected {
        return Err(CodecError::ConversionError(format!(
            "BGR data size mismatch: expected {}, got {}",
            expected,
            bgr.len()
        )));
    }

    // Convert BGR to RGB, then to NV12
    let mut rgb = vec![0u8; w * h * 3];
    for i in 0..(w * h) {
        rgb[i * 3] = bgr[i * 3 + 2]; // R
        rgb[i * 3 + 1] = bgr[i * 3 + 1]; // G
        rgb[i * 3 + 2] = bgr[i * 3]; // B
    }

    rgb_to_nv12(&rgb, width, height)
}

/// Convert BGRA to NV12.
fn bgra_to_nv12(bgra: &[u8], width: u32, height: u32) -> Result<Vec<u8>, CodecError> {
    let w = width as usize;
    let h = height as usize;
    let expected = w * h * 4;
    if bgra.len() != expected {
        return Err(CodecError::ConversionError(format!(
            "BGRA data size mismatch: expected {}, got {}",
            expected,
            bgra.len()
        )));
    }

    // Convert BGRA to RGB, then to NV12
    let mut rgb = vec![0u8; w * h * 3];
    for i in 0..(w * h) {
        rgb[i * 3] = bgra[i * 4 + 2]; // R
        rgb[i * 3 + 1] = bgra[i * 4 + 1]; // G
        rgb[i * 3 + 2] = bgra[i * 4]; // B
    }

    rgb_to_nv12(&rgb, width, height)
}

/// Convert ARGB to NV12.
fn argb_to_nv12(argb: &[u8], width: u32, height: u32) -> Result<Vec<u8>, CodecError> {
    let w = width as usize;
    let h = height as usize;
    let expected = w * h * 4;
    if argb.len() != expected {
        return Err(CodecError::ConversionError(format!(
            "ARGB data size mismatch: expected {}, got {}",
            expected,
            argb.len()
        )));
    }

    // Convert ARGB to RGB, then to NV12
    let mut rgb = vec![0u8; w * h * 3];
    for i in 0..(w * h) {
        rgb[i * 3] = argb[i * 4 + 1]; // R
        rgb[i * 3 + 1] = argb[i * 4 + 2]; // G
        rgb[i * 3 + 2] = argb[i * 4 + 3]; // B
    }

    rgb_to_nv12(&rgb, width, height)
}

/// Convert ABGR to NV12.
fn abgr_to_nv12(abgr: &[u8], width: u32, height: u32) -> Result<Vec<u8>, CodecError> {
    let w = width as usize;
    let h = height as usize;
    let expected = w * h * 4;
    if abgr.len() != expected {
        return Err(CodecError::ConversionError(format!(
            "ABGR data size mismatch: expected {}, got {}",
            expected,
            abgr.len()
        )));
    }

    // Convert ABGR to RGB, then to NV12
    let mut rgb = vec![0u8; w * h * 3];
    for i in 0..(w * h) {
        rgb[i * 3] = abgr[i * 4 + 3]; // R
        rgb[i * 3 + 1] = abgr[i * 4 + 2]; // G
        rgb[i * 3 + 2] = abgr[i * 4 + 1]; // B
    }

    rgb_to_nv12(&rgb, width, height)
}

/// Convert I420 to NV12.
fn i420_to_nv12(i420: &[u8], width: u32, height: u32) -> Result<Vec<u8>, CodecError> {
    let w = width as usize;
    let h = height as usize;
    let y_size = w * h;
    let uv_size = y_size / 4;
    let expected = y_size + uv_size * 2;
    if i420.len() != expected {
        return Err(CodecError::ConversionError(format!(
            "I420 data size mismatch: expected {}, got {}",
            expected,
            i420.len()
        )));
    }

    let mut nv12 = vec![0u8; y_size + y_size / 2];

    // Copy Y plane
    nv12[..y_size].copy_from_slice(&i420[..y_size]);

    // Interleave U and V planes
    let u_plane = &i420[y_size..y_size + uv_size];
    let v_plane = &i420[y_size + uv_size..];
    let uv_offset = y_size;

    for i in 0..uv_size {
        nv12[uv_offset + i * 2] = u_plane[i];
        nv12[uv_offset + i * 2 + 1] = v_plane[i];
    }

    Ok(nv12)
}

/// Convert RGB to ARGB.
fn rgb_to_argb(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>, CodecError> {
    let w = width as usize;
    let h = height as usize;
    let expected = w * h * 3;
    if rgb.len() != expected {
        return Err(CodecError::ConversionError(format!(
            "RGB data size mismatch: expected {}, got {}",
            expected,
            rgb.len()
        )));
    }

    let mut argb = vec![0u8; w * h * 4];
    for i in 0..(w * h) {
        argb[i * 4] = 255; // A
        argb[i * 4 + 1] = rgb[i * 3]; // R
        argb[i * 4 + 2] = rgb[i * 3 + 1]; // G
        argb[i * 4 + 3] = rgb[i * 3 + 2]; // B
    }

    Ok(argb)
}

/// Convert RGBA to ARGB.
fn rgba_to_argb(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, CodecError> {
    let w = width as usize;
    let h = height as usize;
    let expected = w * h * 4;
    if rgba.len() != expected {
        return Err(CodecError::ConversionError(format!(
            "RGBA data size mismatch: expected {}, got {}",
            expected,
            rgba.len()
        )));
    }

    let mut argb = vec![0u8; w * h * 4];
    for i in 0..(w * h) {
        argb[i * 4] = rgba[i * 4 + 3]; // A
        argb[i * 4 + 1] = rgba[i * 4]; // R
        argb[i * 4 + 2] = rgba[i * 4 + 1]; // G
        argb[i * 4 + 3] = rgba[i * 4 + 2]; // B
    }

    Ok(argb)
}

/// Convert BGR to ARGB.
fn bgr_to_argb(bgr: &[u8], width: u32, height: u32) -> Result<Vec<u8>, CodecError> {
    let w = width as usize;
    let h = height as usize;
    let expected = w * h * 3;
    if bgr.len() != expected {
        return Err(CodecError::ConversionError(format!(
            "BGR data size mismatch: expected {}, got {}",
            expected,
            bgr.len()
        )));
    }

    let mut argb = vec![0u8; w * h * 4];
    for i in 0..(w * h) {
        argb[i * 4] = 255; // A
        argb[i * 4 + 1] = bgr[i * 3 + 2]; // R
        argb[i * 4 + 2] = bgr[i * 3 + 1]; // G
        argb[i * 4 + 3] = bgr[i * 3]; // B
    }

    Ok(argb)
}

/// Convert BGRA to ARGB.
fn bgra_to_argb(bgra: &[u8], width: u32, height: u32) -> Result<Vec<u8>, CodecError> {
    let w = width as usize;
    let h = height as usize;
    let expected = w * h * 4;
    if bgra.len() != expected {
        return Err(CodecError::ConversionError(format!(
            "BGRA data size mismatch: expected {}, got {}",
            expected,
            bgra.len()
        )));
    }

    let mut argb = vec![0u8; w * h * 4];
    for i in 0..(w * h) {
        argb[i * 4] = bgra[i * 4 + 3]; // A
        argb[i * 4 + 1] = bgra[i * 4 + 2]; // R
        argb[i * 4 + 2] = bgra[i * 4 + 1]; // G
        argb[i * 4 + 3] = bgra[i * 4]; // B
    }

    Ok(argb)
}

// ============================================================================
// xoq::Frame integration
// ============================================================================

#[cfg(feature = "xoq-frame")]
impl From<xoq::Frame> for VideoFrame {
    fn from(frame: xoq::Frame) -> Self {
        VideoFrame::new(
            frame.width,
            frame.height,
            PixelFormat::Rgb, // xoq::Frame uses RGB
            frame.data,
            frame.timestamp_us,
        )
    }
}

#[cfg(feature = "xoq-frame")]
impl From<VideoFrame> for xoq::Frame {
    fn from(frame: VideoFrame) -> Self {
        // Convert to RGB if necessary
        let rgb_frame = if frame.pixel_format == PixelFormat::Rgb {
            frame
        } else {
            // For now, just use the data as-is (may need conversion)
            frame
        };

        xoq::Frame {
            width: rgb_frame.width,
            height: rgb_frame.height,
            data: rgb_frame.data,
            timestamp_us: rgb_frame.timestamp_us,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expected_frame_size() {
        assert_eq!(expected_frame_size(1920, 1080, PixelFormat::Rgb), 1920 * 1080 * 3);
        assert_eq!(expected_frame_size(1920, 1080, PixelFormat::Rgba), 1920 * 1080 * 4);
        assert_eq!(expected_frame_size(1920, 1080, PixelFormat::Nv12), 1920 * 1080 * 3 / 2);
    }

    #[test]
    fn test_rgb_to_nv12() {
        // Create a simple 2x2 red image
        let rgb = vec![
            255, 0, 0, // red
            255, 0, 0, // red
            255, 0, 0, // red
            255, 0, 0, // red
        ];

        let nv12 = rgb_to_nv12(&rgb, 2, 2).unwrap();
        assert_eq!(nv12.len(), 2 * 2 + 2); // Y plane + UV plane

        // Y values should be around 82 for pure red (BT.601)
        assert!(nv12[0] > 60 && nv12[0] < 100);
    }

    #[test]
    fn test_i420_to_nv12() {
        // Create a 4x4 I420 frame
        let y_plane: Vec<u8> = vec![16; 16]; // 4x4 Y
        let u_plane: Vec<u8> = vec![128; 4]; // 2x2 U
        let v_plane: Vec<u8> = vec![128; 4]; // 2x2 V

        let mut i420 = y_plane.clone();
        i420.extend(&u_plane);
        i420.extend(&v_plane);

        let nv12 = i420_to_nv12(&i420, 4, 4).unwrap();
        assert_eq!(nv12.len(), 16 + 8); // Y plane + interleaved UV

        // Check Y plane is copied
        assert_eq!(&nv12[..16], &y_plane[..]);

        // Check UV is interleaved
        assert_eq!(nv12[16], 128); // U
        assert_eq!(nv12[17], 128); // V
    }
}

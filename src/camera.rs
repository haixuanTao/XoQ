//! Camera capture functionality using V4L2.
//!
//! This module provides camera access using the v4l crate (Video4Linux2).
//! Unlike nokhwa, v4l types are Send, allowing simpler async architectures.
//!
//! # Example
//!
//! ```rust,no_run
//! use xoq::camera::{Camera, list_cameras};
//!
//! // List available cameras
//! let cameras = list_cameras().unwrap();
//! for cam in &cameras {
//!     println!("Camera: {} (index {})", cam.name, cam.index);
//! }
//!
//! // Open a camera
//! let mut camera = Camera::open(0, 640, 480, 30).unwrap();
//!
//! // Capture a frame
//! let frame = camera.capture().unwrap();
//! println!("Frame: {}x{}, {} bytes", frame.width, frame.height, frame.data.len());
//! ```

use anyhow::Result;
use std::path::PathBuf;
use v4l::buffer::Type;
use v4l::io::mmap::Stream;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::{Device, FourCC};

/// Information about an available camera.
#[derive(Debug, Clone)]
pub struct CameraInfo {
    /// Camera index (used to open the camera).
    pub index: u32,
    /// Human-readable camera name.
    pub name: String,
    /// Device path (e.g., /dev/video0).
    pub path: PathBuf,
}

/// A captured video frame.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Raw RGB data (3 bytes per pixel, row-major).
    pub data: Vec<u8>,
    /// Frame timestamp in microseconds since capture start.
    pub timestamp_us: u64,
}

/// Raw captured frame before conversion (for hardware encoding).
#[derive(Debug, Clone)]
pub struct RawFrame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Raw data in capture format (YUYV or MJPEG).
    pub data: Vec<u8>,
    /// Capture format.
    pub format: RawFormat,
    /// Frame timestamp in microseconds since capture start.
    pub timestamp_us: u64,
}

/// Raw capture format.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RawFormat {
    /// YUYV (YUV 4:2:2) - good for hardware encoding
    Yuyv,
    /// MJPEG - compressed, needs decode for encoding
    Mjpeg,
}

impl Frame {
    /// Convert frame to JPEG bytes.
    pub fn to_jpeg(&self, quality: u8) -> Result<Vec<u8>> {
        use image::{ImageBuffer, Rgb};

        let img: ImageBuffer<Rgb<u8>, _> =
            ImageBuffer::from_raw(self.width, self.height, self.data.clone())
                .ok_or_else(|| anyhow::anyhow!("Failed to create image buffer"))?;

        let mut jpeg_data = Vec::new();
        let mut encoder =
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_data, quality);
        encoder.encode_image(&img)?;

        Ok(jpeg_data)
    }

    /// Create a frame from JPEG bytes.
    pub fn from_jpeg(jpeg_data: &[u8]) -> Result<Self> {
        use image::ImageReader;
        use std::io::Cursor;

        let img = ImageReader::new(Cursor::new(jpeg_data))
            .with_guessed_format()?
            .decode()?
            .to_rgb8();

        Ok(Frame {
            width: img.width(),
            height: img.height(),
            data: img.into_raw(),
            timestamp_us: 0,
        })
    }
}

/// A camera capture device using V4L2.
pub struct Camera {
    stream: Stream<'static>,
    width: u32,
    height: u32,
    format: CaptureFormat,
    start_time: std::time::Instant,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum CaptureFormat {
    Mjpeg,
    Yuyv,
}

// Camera is Send because v4l types are Send
unsafe impl Send for Camera {}

/// Options for opening a camera.
#[derive(Debug, Clone, Default)]
pub struct CameraOptions {
    /// Prefer YUYV format (better for hardware encoding).
    /// Default: false (prefer MJPEG for CPU efficiency)
    pub prefer_yuyv: bool,
}

impl Camera {
    /// Open a camera by index with specified resolution and framerate.
    ///
    /// # Arguments
    ///
    /// * `index` - Camera index (0 for first camera)
    /// * `width` - Requested frame width
    /// * `height` - Requested frame height
    /// * `fps` - Requested frames per second
    pub fn open(index: u32, width: u32, height: u32, fps: u32) -> Result<Self> {
        let path = format!("/dev/video{}", index);
        Self::open_path(&path, width, height, fps)
    }

    /// Open a camera with options.
    pub fn open_with_options(
        index: u32,
        width: u32,
        height: u32,
        fps: u32,
        options: CameraOptions,
    ) -> Result<Self> {
        let path = format!("/dev/video{}", index);
        Self::open_path_with_options(&path, width, height, fps, options)
    }

    /// Open a camera by device path.
    pub fn open_path(path: &str, width: u32, height: u32, fps: u32) -> Result<Self> {
        Self::open_path_with_options(path, width, height, fps, CameraOptions::default())
    }

    /// Open a camera by device path with options.
    pub fn open_path_with_options(
        path: &str,
        width: u32,
        height: u32,
        fps: u32,
        options: CameraOptions,
    ) -> Result<Self> {
        let device = Device::with_path(path)?;

        let (format, capture_format) =
            Self::try_set_format(&device, width, height, fps, options.prefer_yuyv)?;

        let width = format.width;
        let height = format.height;

        // Create stream with buffers - need to leak the device for 'static lifetime
        let device = Box::leak(Box::new(device));
        let stream = Stream::with_buffers(device, Type::VideoCapture, 4)?;

        Ok(Camera {
            stream,
            width,
            height,
            format: capture_format,
            start_time: std::time::Instant::now(),
        })
    }

    fn try_set_format(
        device: &Device,
        width: u32,
        height: u32,
        _fps: u32,
        prefer_yuyv: bool,
    ) -> Result<(v4l::Format, CaptureFormat)> {
        let mut format = device.format()?;
        format.width = width;
        format.height = height;

        if prefer_yuyv {
            // Try YUYV first (better for hardware encoding)
            format.fourcc = FourCC::new(b"YUYV");
            if let Ok(f) = device.set_format(&format) {
                if f.fourcc == FourCC::new(b"YUYV") {
                    return Ok((f, CaptureFormat::Yuyv));
                }
            }
            // Fall back to MJPEG
            format.fourcc = FourCC::new(b"MJPG");
            if let Ok(f) = device.set_format(&format) {
                if f.fourcc == FourCC::new(b"MJPG") {
                    return Ok((f, CaptureFormat::Mjpeg));
                }
            }
        } else {
            // Try MJPEG first (more efficient for CPU)
            format.fourcc = FourCC::new(b"MJPG");
            if let Ok(f) = device.set_format(&format) {
                if f.fourcc == FourCC::new(b"MJPG") {
                    return Ok((f, CaptureFormat::Mjpeg));
                }
            }
            // Fall back to YUYV
            format.fourcc = FourCC::new(b"YUYV");
            if let Ok(f) = device.set_format(&format) {
                if f.fourcc == FourCC::new(b"YUYV") {
                    return Ok((f, CaptureFormat::Yuyv));
                }
            }
        }

        // Accept whatever the camera gives us
        let f = device.format()?;
        if f.fourcc == FourCC::new(b"MJPG") {
            Ok((f, CaptureFormat::Mjpeg))
        } else {
            Ok((f, CaptureFormat::Yuyv))
        }
    }

    /// Get the actual frame width.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Get the actual frame height.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Get the capture format.
    pub fn format_name(&self) -> &'static str {
        match self.format {
            CaptureFormat::Mjpeg => "MJPEG",
            CaptureFormat::Yuyv => "YUYV",
        }
    }

    /// Capture a single frame (converted to RGB).
    pub fn capture(&mut self) -> Result<Frame> {
        let (data, _meta) = self.stream.next()?;
        let timestamp_us = self.start_time.elapsed().as_micros() as u64;

        let rgb_data = match self.format {
            CaptureFormat::Mjpeg => {
                // Decode MJPEG to RGB
                Self::mjpeg_to_rgb(data, self.width, self.height)?
            }
            CaptureFormat::Yuyv => {
                // Convert YUYV to RGB
                Self::yuyv_to_rgb(data, self.width, self.height)
            }
        };

        Ok(Frame {
            width: self.width,
            height: self.height,
            data: rgb_data,
            timestamp_us,
        })
    }

    /// Capture a raw frame without conversion (for hardware encoding).
    pub fn capture_raw(&mut self) -> Result<RawFrame> {
        let (data, _meta) = self.stream.next()?;
        let timestamp_us = self.start_time.elapsed().as_micros() as u64;

        let raw_format = match self.format {
            CaptureFormat::Mjpeg => RawFormat::Mjpeg,
            CaptureFormat::Yuyv => RawFormat::Yuyv,
        };

        Ok(RawFrame {
            width: self.width,
            height: self.height,
            data: data.to_vec(),
            format: raw_format,
            timestamp_us,
        })
    }

    /// Check if the camera is capturing in YUYV format (good for hardware encoding).
    pub fn is_yuyv(&self) -> bool {
        self.format == CaptureFormat::Yuyv
    }

    fn mjpeg_to_rgb(data: &[u8], _width: u32, _height: u32) -> Result<Vec<u8>> {
        use image::ImageReader;
        use std::io::Cursor;

        let img = ImageReader::new(Cursor::new(data))
            .with_guessed_format()?
            .decode()?
            .to_rgb8();

        Ok(img.into_raw())
    }

    fn yuyv_to_rgb(yuyv: &[u8], width: u32, height: u32) -> Vec<u8> {
        let width = width as usize;
        let height = height as usize;
        let mut rgb = vec![0u8; width * height * 3];

        for y in 0..height {
            for x in (0..width).step_by(2) {
                let yuyv_idx = (y * width + x) * 2;
                let y0 = yuyv.get(yuyv_idx).copied().unwrap_or(0) as f32;
                let u = yuyv.get(yuyv_idx + 1).copied().unwrap_or(128) as f32;
                let y1 = yuyv.get(yuyv_idx + 2).copied().unwrap_or(0) as f32;
                let v = yuyv.get(yuyv_idx + 3).copied().unwrap_or(128) as f32;

                // YUV to RGB conversion (BT.601)
                let c0 = y0 - 16.0;
                let c1 = y1 - 16.0;
                let d = u - 128.0;
                let e = v - 128.0;

                // First pixel
                let r0 = (1.164 * c0 + 1.596 * e).clamp(0.0, 255.0) as u8;
                let g0 = (1.164 * c0 - 0.392 * d - 0.813 * e).clamp(0.0, 255.0) as u8;
                let b0 = (1.164 * c0 + 2.017 * d).clamp(0.0, 255.0) as u8;

                // Second pixel
                let r1 = (1.164 * c1 + 1.596 * e).clamp(0.0, 255.0) as u8;
                let g1 = (1.164 * c1 - 0.392 * d - 0.813 * e).clamp(0.0, 255.0) as u8;
                let b1 = (1.164 * c1 + 2.017 * d).clamp(0.0, 255.0) as u8;

                let rgb_idx0 = (y * width + x) * 3;
                let rgb_idx1 = (y * width + x + 1) * 3;

                if rgb_idx1 + 2 < rgb.len() {
                    rgb[rgb_idx0] = r0;
                    rgb[rgb_idx0 + 1] = g0;
                    rgb[rgb_idx0 + 2] = b0;
                    rgb[rgb_idx1] = r1;
                    rgb[rgb_idx1 + 1] = g1;
                    rgb[rgb_idx1 + 2] = b1;
                }
            }
        }

        rgb
    }
}

/// List all available cameras.
pub fn list_cameras() -> Result<Vec<CameraInfo>> {
    let mut cameras = Vec::new();

    for entry in std::fs::read_dir("/dev")? {
        let entry = entry?;
        let path = entry.path();

        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with("video") {
                if let Ok(index) = name[5..].parse::<u32>() {
                    // Try to get device info
                    let device_name = if let Ok(device) = Device::with_path(&path) {
                        device
                            .query_caps()
                            .map(|c| c.card)
                            .unwrap_or_else(|_| format!("Camera {}", index))
                    } else {
                        format!("Camera {}", index)
                    };

                    cameras.push(CameraInfo {
                        index,
                        name: device_name,
                        path,
                    });
                }
            }
        }
    }

    cameras.sort_by_key(|c| c.index);
    Ok(cameras)
}

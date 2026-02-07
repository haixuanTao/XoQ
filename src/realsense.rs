//! Intel RealSense depth camera capture wrapper.
//!
//! Provides a thin wrapper around `realsense-rust` for capturing aligned
//! color (RGB) + depth (u16 mm) frames from RealSense cameras.

use anyhow::Result;
use realsense_rust::{
    config::Config,
    context::Context,
    frame::{ColorFrame, CompositeFrame, DepthFrame, FrameEx},
    kind::{Rs2CameraInfo, Rs2Format, Rs2ProductLine, Rs2StreamKind},
    pipeline::InactivePipeline,
    processing_blocks::align::Align,
};
use std::collections::HashSet;
use std::ffi::CString;
use std::time::Duration;

/// Captured frames from a RealSense camera.
pub struct RealSenseFrames {
    /// RGB color data (width * height * 3 bytes).
    pub color_rgb: Vec<u8>,
    /// Raw depth values in millimeters (width * height u16 values).
    pub depth_mm: Vec<u16>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Frame timestamp in microseconds.
    pub timestamp_us: u64,
}

/// Camera intrinsics for depth-to-3D projection.
#[derive(Debug, Clone, Copy)]
pub struct Intrinsics {
    pub fx: f32,
    pub fy: f32,
    pub ppx: f32,
    pub ppy: f32,
}

/// RealSense camera wrapper with aligned color + depth capture.
pub struct RealSenseCamera {
    pipeline: realsense_rust::pipeline::ActivePipeline,
    align: Align,
    width: u32,
    height: u32,
    /// Depth scale: multiply raw Z16 value by this to get meters.
    depth_scale: f32,
    /// Color stream intrinsics (depth is aligned to color).
    intrinsics: Intrinsics,
}

impl RealSenseCamera {
    /// List connected RealSense devices. Returns vec of (name, serial).
    pub fn list_devices() -> Result<Vec<(String, String)>> {
        let context = Context::new()?;
        let mut product_lines = HashSet::new();
        product_lines.insert(Rs2ProductLine::AnyIntel);
        let devices = context.query_devices(product_lines);
        let mut result = Vec::new();
        for device in &devices {
            let name = device
                .info(Rs2CameraInfo::Name)
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "Unknown".to_string());
            let serial = device
                .info(Rs2CameraInfo::SerialNumber)
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "Unknown".to_string());
            result.push((name, serial));
        }
        Ok(result)
    }

    /// Open a RealSense camera with color + depth streams.
    /// If `serial` is Some, opens the device with that serial number.
    pub fn open(width: u32, height: u32, fps: u32, serial: Option<&str>) -> Result<Self> {
        let context = Context::new()?;
        let pipeline = InactivePipeline::try_from(&context)?;

        let mut config = Config::new();
        if let Some(sn) = serial {
            let c_serial = CString::new(sn)?;
            config.enable_device_from_serial(&c_serial)?;
        }
        config.enable_stream(
            Rs2StreamKind::Color,
            None,
            width as usize,
            height as usize,
            Rs2Format::Rgb8,
            fps as usize,
        )?;
        config.enable_stream(
            Rs2StreamKind::Depth,
            None,
            width as usize,
            height as usize,
            Rs2Format::Z16,
            fps as usize,
        )?;

        let pipeline = pipeline.start(Some(config))?;

        // Query depth scale from the depth sensor
        let mut depth_scale = 0.001f32; // default: 1mm per unit
        let device = pipeline.profile().device();
        for sensor in device.sensors() {
            if let Some(val) = sensor.get_option(realsense_rust::kind::Rs2Option::DepthUnits) {
                depth_scale = val;
                break;
            }
        }

        // Query color stream intrinsics (depth is aligned to color frame)
        let mut intrinsics = Intrinsics {
            fx: 383.0,
            fy: 383.0,
            ppx: width as f32 / 2.0,
            ppy: height as f32 / 2.0,
        };
        for stream in pipeline.profile().streams() {
            if stream.kind() == Rs2StreamKind::Color {
                if let Ok(intr) = stream.intrinsics() {
                    intrinsics = Intrinsics {
                        fx: intr.fx(),
                        fy: intr.fy(),
                        ppx: intr.ppx(),
                        ppy: intr.ppy(),
                    };
                }
                break;
            }
        }

        // Align depth frames to color coordinate space
        let align = Align::new(Rs2StreamKind::Color, 1)?;

        Ok(Self {
            pipeline,
            align,
            width,
            height,
            depth_scale,
            intrinsics,
        })
    }

    /// Capture aligned color + depth frames.
    pub fn capture(&mut self) -> Result<RealSenseFrames> {
        let composite: CompositeFrame = self.pipeline.wait(Some(Duration::from_secs(5)))?;

        // Align depth to color
        self.align.queue(composite)?;
        let aligned = self.align.wait(Duration::from_secs(5))?;

        // Extract color frame
        let color_frames: Vec<ColorFrame> = aligned.frames_of_type();
        let color_frame = color_frames
            .first()
            .ok_or_else(|| anyhow::anyhow!("No color frame in composite"))?;

        let color_width = color_frame.width() as u32;
        let color_height = color_frame.height() as u32;
        let timestamp_us = (color_frame.timestamp() * 1000.0) as u64; // ms → us

        // Get raw color data (RGB8)
        let color_data_size = color_frame.get_data_size();
        let color_rgb = unsafe {
            let ptr = color_frame.get_data() as *const std::ffi::c_void as *const u8;
            std::slice::from_raw_parts(ptr, color_data_size).to_vec()
        };

        // Extract depth frame
        let depth_frames: Vec<DepthFrame> = aligned.frames_of_type();
        let depth_frame = depth_frames
            .first()
            .ok_or_else(|| anyhow::anyhow!("No depth frame in composite"))?;

        // Get raw depth data (Z16) and convert to millimeters using depth_scale
        let depth_data_size = depth_frame.get_data_size();
        let depth_mm = unsafe {
            let ptr = depth_frame.get_data() as *const std::ffi::c_void as *const u16;
            let count = depth_data_size / 2;
            let raw = std::slice::from_raw_parts(ptr, count);
            let scale_to_mm = self.depth_scale * 1000.0;
            raw.iter()
                .map(|&v| (v as f32 * scale_to_mm) as u16)
                .collect()
        };

        Ok(RealSenseFrames {
            color_rgb,
            depth_mm,
            width: color_width,
            height: color_height,
            timestamp_us,
        })
    }

    /// Get configured width.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Get configured height.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Get depth scale (meters per raw unit).
    pub fn depth_scale(&self) -> f32 {
        self.depth_scale
    }

    /// Get camera intrinsics (from color stream, depth is aligned to it).
    pub fn intrinsics(&self) -> Intrinsics {
        self.intrinsics
    }
}

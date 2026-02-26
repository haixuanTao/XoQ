//! Intel RealSense depth camera capture wrapper.
//!
//! Provides a thin wrapper around `realsense-rust` for capturing aligned
//! color (RGB) + depth (u16 mm) frames from RealSense cameras.

use anyhow::Result;
use realsense_rust::{
    config::Config,
    context::Context,
    frame::{AccelFrame, ColorFrame, CompositeFrame, DepthFrame, FrameEx},
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
    /// Latest accelerometer reading [ax, ay, az] in m/s^2, if available (D435i IMU).
    pub accel: Option<[f32; 3]>,
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
    /// Latest accelerometer reading [ax, ay, az] in m/s^2 (D435i IMU).
    last_accel: Option<[f32; 3]>,
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

        // Try with accel stream first (D435i IMU), fall back without it (D435).
        // config.enable_stream(Accel) can succeed even on non-IMU cameras,
        // but pipeline.start() will fail when it can't resolve the config.
        let (pipeline, has_accel) = {
            let pipeline = InactivePipeline::try_from(&context)?;
            let mut config = Config::new();
            if let Some(sn) = serial {
                config.enable_device_from_serial(&CString::new(sn)?)?;
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
            let _ = config.enable_stream(Rs2StreamKind::Accel, None, 0, 0, Rs2Format::Any, 0);

            match pipeline.start(Some(config)) {
                Ok(p) => (p, true),
                Err(_) => {
                    // Accel not supported — retry without it
                    let pipeline = InactivePipeline::try_from(&context)?;
                    let mut config = Config::new();
                    if let Some(sn) = serial {
                        config.enable_device_from_serial(&CString::new(sn)?)?;
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
                    (pipeline.start(Some(config))?, false)
                }
            }
        };

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

        if has_accel {
            tracing::info!("IMU accelerometer stream enabled (gravity correction available)");
        }

        Ok(Self {
            pipeline,
            align,
            width,
            height,
            depth_scale,
            intrinsics,
            last_accel: None,
        })
    }

    /// Capture aligned color + depth frames (+ accelerometer if available).
    pub fn capture(&mut self) -> Result<RealSenseFrames> {
        let composite: CompositeFrame = self.pipeline.wait(Some(Duration::from_secs(5)))?;

        // Extract accel frames before alignment (IMU frames are independent of depth/color).
        // The accel stream runs at 100-200 Hz vs 15 fps for depth/color, so not every
        // composite will contain an accel frame — we keep the last known value.
        let accel_frames: Vec<AccelFrame> = composite.frames_of_type();
        if let Some(af) = accel_frames.first() {
            self.last_accel = Some(*af.acceleration());
        }
        drop(accel_frames);

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
            accel: self.last_accel,
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

    /// Run On-Chip Calibration (OCC) on a dedicated 256x144@90fps depth-only pipeline.
    ///
    /// Call this BEFORE `open()` — it creates and destroys its own pipeline.
    /// Uses the calibration-specific resolution that the D400 ASIC requires.
    ///
    /// Returns `(health, applied)` where health is the calibration health metric
    /// and applied indicates whether new calibration was written to EEPROM.
    /// Health < 0.25 means calibration is already good; >= 0.25 triggers recalibration.
    pub fn run_on_chip_calibration(serial: Option<&str>) -> Result<(f32, bool)> {
        use realsense_sys::*;

        tracing::info!("Starting OCC with dedicated calibration pipeline (256x144@90fps)...");

        let mut pipeline = start_calibration_pipeline(serial)?;

        // Wait for depth frames to flow + let firmware settle
        tracing::info!("Warming up calibration pipeline (30 frames)...");
        for _ in 0..30 {
            let _ = pipeline.wait(Some(Duration::from_millis(500)));
        }
        std::thread::sleep(Duration::from_secs(1));

        unsafe {
            let mut err: *mut rs2_error = std::ptr::null_mut();
            let device = raw_device_from_pipeline(&pipeline);
            if device.is_null() {
                return Err(anyhow::anyhow!("Pipeline device pointer is null"));
            }

            let json_config = b"{\"speed\": 2}";
            let mut health: f32 = 0.0;

            tracing::info!("Running on-chip calibration (speed=2, ~15s)...");

            let calib_buf = rs2_run_on_chip_calibration(
                device,
                json_config.as_ptr() as *const std::ffi::c_void,
                json_config.len() as i32,
                &mut health,
                None,
                std::ptr::null_mut(),
                30000,
                &mut err,
            );

            if !err.is_null() {
                let msg = get_rs2_error_message(err);
                rs2_free_error(err);
                return Err(anyhow::anyhow!("OCC failed: {}", msg));
            }

            tracing::info!("OCC completed, health: {:.3}", health);

            let applied = if health >= 0.25 {
                if !calib_buf.is_null() {
                    if let Err(e) = apply_calibration(device, calib_buf) {
                        tracing::warn!("Failed to apply OCC calibration: {}", e);
                    }
                }
                true
            } else {
                false
            };

            if !calib_buf.is_null() {
                rs2_delete_raw_data(calib_buf);
            }

            Ok((health, applied))
        }
        // pipeline drops here, releasing the calibration stream
    }

    /// Run Tare Calibration on a dedicated 256x144@90fps depth-only pipeline.
    ///
    /// Call this BEFORE `open()`. Point the camera at a flat surface at a known
    /// distance and provide `ground_truth_mm` (the true distance in millimeters).
    ///
    /// Returns `(health, applied)` — calibration is always applied if successful.
    pub fn run_tare_calibration(serial: Option<&str>, ground_truth_mm: f32) -> Result<(f32, bool)> {
        use realsense_sys::*;

        tracing::info!(
            "Starting tare calibration with dedicated pipeline (256x144@90fps, target={}mm)...",
            ground_truth_mm
        );

        let mut pipeline = start_calibration_pipeline(serial)?;

        tracing::info!("Warming up calibration pipeline (30 frames)...");
        for _ in 0..30 {
            let _ = pipeline.wait(Some(Duration::from_millis(500)));
        }
        std::thread::sleep(Duration::from_secs(1));

        unsafe {
            let mut err: *mut rs2_error = std::ptr::null_mut();
            let device = raw_device_from_pipeline(&pipeline);
            if device.is_null() {
                return Err(anyhow::anyhow!("Pipeline device pointer is null"));
            }

            let json_config = b"{\"speed\": 2}";
            let mut health: f32 = 0.0;

            tracing::info!(
                "Running tare calibration (ground_truth={}mm, speed=2)...",
                ground_truth_mm
            );

            let calib_buf = rs2_run_tare_calibration(
                device,
                ground_truth_mm,
                json_config.as_ptr() as *const std::ffi::c_void,
                json_config.len() as i32,
                &mut health,
                None,
                std::ptr::null_mut(),
                30000,
                &mut err,
            );

            if !err.is_null() {
                let msg = get_rs2_error_message(err);
                rs2_free_error(err);
                return Err(anyhow::anyhow!("Tare calibration failed: {}", msg));
            }

            tracing::info!("Tare calibration completed, health: {:.3}", health);

            let mut applied = false;
            if !calib_buf.is_null() {
                match apply_calibration(device, calib_buf) {
                    Ok(()) => {
                        tracing::info!("Tare calibration applied and persisted to EEPROM");
                        applied = true;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to apply tare calibration: {}", e);
                    }
                }
                rs2_delete_raw_data(calib_buf);
            }

            Ok((health, applied))
        }
    }
}

/// Start a dedicated calibration pipeline at 256x144@90fps depth-only.
/// The D400 ASIC requires this specific configuration for OCC/tare calibration.
fn start_calibration_pipeline(
    serial: Option<&str>,
) -> Result<realsense_rust::pipeline::ActivePipeline> {
    let context = Context::new()?;
    let pipeline = InactivePipeline::try_from(&context)?;

    let mut config = Config::new();
    if let Some(sn) = serial {
        let c_serial = CString::new(sn)?;
        config.enable_device_from_serial(&c_serial)?;
    }
    config.enable_stream(Rs2StreamKind::Depth, None, 256, 144, Rs2Format::Z16, 90)?;

    let pipeline = pipeline.start(Some(config))?;
    Ok(pipeline)
}

/// Extract the raw `*mut rs2_device` pointer from an active pipeline.
/// The `Device` struct has a single `NonNull<rs2_device>` field.
/// The returned pointer is borrowed — do NOT call `rs2_delete_device` on it.
unsafe fn raw_device_from_pipeline(
    pipeline: &realsense_rust::pipeline::ActivePipeline,
) -> *mut realsense_sys::rs2_device {
    let rs_device = pipeline.profile().device();
    std::mem::transmute_copy::<realsense_rust::device::Device, *mut realsense_sys::rs2_device>(
        rs_device,
    )
}

/// Apply a calibration buffer: set the calibration table, then persist to EEPROM.
unsafe fn apply_calibration(
    device: *mut realsense_sys::rs2_device,
    calib_buf: *const realsense_sys::rs2_raw_data_buffer,
) -> Result<()> {
    use realsense_sys::*;
    let mut err: *mut rs2_error = std::ptr::null_mut();

    let buf_size = rs2_get_raw_data_size(calib_buf, &mut err);
    if !err.is_null() {
        let msg = get_rs2_error_message(err);
        rs2_free_error(err);
        return Err(anyhow::anyhow!(
            "Failed to get calibration data size: {}",
            msg
        ));
    }
    if buf_size == 0 {
        return Err(anyhow::anyhow!("Calibration buffer is empty"));
    }

    let buf_ptr = rs2_get_raw_data(calib_buf, &mut err);
    if !err.is_null() || buf_ptr.is_null() {
        if !err.is_null() {
            let msg = get_rs2_error_message(err);
            rs2_free_error(err);
            return Err(anyhow::anyhow!("Failed to get calibration data: {}", msg));
        }
        return Err(anyhow::anyhow!("Calibration data pointer is null"));
    }

    rs2_set_calibration_table(
        device,
        buf_ptr as *const std::ffi::c_void,
        buf_size,
        &mut err,
    );
    if !err.is_null() {
        let msg = get_rs2_error_message(err);
        rs2_free_error(err);
        return Err(anyhow::anyhow!("Failed to set calibration table: {}", msg));
    }

    rs2_write_calibration(device, &mut err);
    if !err.is_null() {
        let msg = get_rs2_error_message(err);
        rs2_free_error(err);
        return Err(anyhow::anyhow!(
            "Failed to write calibration to EEPROM: {}",
            msg
        ));
    }

    Ok(())
}

/// Extract error message string from an rs2_error pointer.
unsafe fn get_rs2_error_message(err: *const realsense_sys::rs2_error) -> String {
    let ptr = realsense_sys::rs2_get_error_message(err);
    if ptr.is_null() {
        "unknown error".to_string()
    } else {
        std::ffi::CStr::from_ptr(ptr).to_string_lossy().to_string()
    }
}

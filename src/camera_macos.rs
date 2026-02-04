//! Camera capture functionality using AVFoundation (macOS).
//!
//! This module provides camera access using AVFoundation on macOS.
//! It exports the same public API as the V4L2 `camera` module.
//!
//! # Example
//!
//! ```rust,no_run
//! use xoq::camera_macos::{Camera, list_cameras};
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
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2::{class, msg_send};
use objc2_av_foundation::{
    AVCaptureDevice, AVCaptureDeviceInput, AVCaptureSession, AVCaptureVideoDataOutput,
    AVMediaTypeVideo,
};
use objc2_foundation::{NSNumber, NSObject, NSString};
use std::path::PathBuf;
use std::sync::mpsc;

use crate::frame::Frame;

/// Raw captured frame before conversion (for hardware encoding).
#[derive(Debug, Clone)]
pub struct RawFrame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Raw data in capture format (BGRA).
    pub data: Vec<u8>,
    /// Capture format.
    pub format: RawFormat,
    /// Frame timestamp in microseconds since capture start.
    pub timestamp_us: u64,
}

/// Raw capture format.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RawFormat {
    /// BGRA (32-bit) - macOS AVFoundation native format
    Bgra,
}

/// A retained CVPixelBuffer for zero-copy access.
///
/// Holds a CFRetain'd CVPixelBuffer pointer that is automatically released on drop.
/// Use this with `VtEncoder::encode_pixel_buffer()` for zero-copy H.264 encoding.
pub struct RetainedPixelBuffer {
    ptr: *const std::ffi::c_void,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Bytes per row (may include stride padding).
    pub bytes_per_row: usize,
    /// Frame timestamp in microseconds since capture start.
    pub timestamp_us: u64,
}

// Safety: CVPixelBuffer is a CFType with thread-safe reference counting.
// After CFRetain, it can be safely sent to another thread.
unsafe impl Send for RetainedPixelBuffer {}

impl RetainedPixelBuffer {
    /// Get the raw CVPixelBuffer pointer for passing to VideoToolbox APIs.
    pub fn as_ptr(&self) -> *const std::ffi::c_void {
        self.ptr
    }
}

impl Drop for RetainedPixelBuffer {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { CFRelease(self.ptr) };
        }
    }
}

/// Information about an available camera.
#[derive(Debug, Clone)]
pub struct CameraInfo {
    /// Camera index (used to open the camera).
    pub index: u32,
    /// Human-readable camera name.
    pub name: String,
    /// Device unique ID.
    pub path: PathBuf,
}

/// Options for opening a camera.
#[derive(Debug, Clone, Default)]
pub struct CameraOptions {
    /// Unused on macOS (AVFoundation always outputs BGRA).
    pub prefer_yuyv: bool,
}

/// A camera capture device using AVFoundation.
pub struct Camera {
    width: u32,
    height: u32,
    rx: mpsc::Receiver<RetainedPixelBuffer>,
    start_time: std::time::Instant,
    // Keep these alive for the duration of capture
    _session: Retained<AVCaptureSession>,
    _delegate: Retained<NSObject>,
}

// Safety: The AVFoundation objects are accessed through the mpsc channel
// which provides synchronization. The capture session runs on its own
// dispatch queue.
unsafe impl Send for Camera {}

// FFI for CoreFoundation retain/release
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRetain(cf: *const std::ffi::c_void) -> *const std::ffi::c_void;
    fn CFRelease(cf: *const std::ffi::c_void);
}

// FFI for CVPixelBuffer access
#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn CVPixelBufferLockBaseAddress(pixel_buffer: *const std::ffi::c_void, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(pixel_buffer: *const std::ffi::c_void, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddress(pixel_buffer: *const std::ffi::c_void) -> *const u8;
    fn CVPixelBufferGetBytesPerRow(pixel_buffer: *const std::ffi::c_void) -> usize;
    fn CVPixelBufferGetWidth(pixel_buffer: *const std::ffi::c_void) -> usize;
    fn CVPixelBufferGetHeight(pixel_buffer: *const std::ffi::c_void) -> usize;
}

// FFI for CMSampleBuffer pixel buffer extraction
#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetImageBuffer(
        sample_buffer: *const std::ffi::c_void,
    ) -> *const std::ffi::c_void;
}

// Dispatch queue creation
#[link(name = "System")]
extern "C" {
    fn dispatch_queue_create(
        label: *const i8,
        attr: *const std::ffi::c_void,
    ) -> *mut std::ffi::c_void;
}

// ObjC runtime for adding methods
#[link(name = "objc", kind = "dylib")]
extern "C" {
    fn class_addMethod(
        cls: *const std::ffi::c_void,
        name: objc2::runtime::Sel,
        imp: *const std::ffi::c_void,
        types: *const i8,
    ) -> Bool;
}

/// Global sender for capture delegate callback.
/// This is set before starting the capture session and cleared on drop.
static FRAME_SENDER: std::sync::Mutex<Option<mpsc::SyncSender<RetainedPixelBuffer>>> =
    std::sync::Mutex::new(None);

/// kCVPixelBufferLock_ReadOnly flag for read-only locking.
const K_CV_PIXEL_BUFFER_LOCK_READ_ONLY: u64 = 0x00000001;

/// AVFoundation capture delegate callback.
/// Called on the dispatch queue when a new sample buffer is available.
///
/// Instead of copying pixel data, we CFRetain the CVPixelBuffer and send the
/// retained pointer through the channel. This eliminates the per-frame copy
/// in the capture callback hot path.
extern "C" fn capture_callback(
    _this: *mut std::ffi::c_void,
    _cmd: objc2::runtime::Sel,
    _output: *mut std::ffi::c_void,
    sample_buffer: *mut std::ffi::c_void,
    _connection: *mut std::ffi::c_void,
) {
    if sample_buffer.is_null() {
        return;
    }

    unsafe {
        let pixel_buffer = CMSampleBufferGetImageBuffer(sample_buffer);
        if pixel_buffer.is_null() {
            return;
        }

        let width = CVPixelBufferGetWidth(pixel_buffer) as u32;
        let height = CVPixelBufferGetHeight(pixel_buffer) as u32;
        let bytes_per_row = CVPixelBufferGetBytesPerRow(pixel_buffer);

        // Retain the pixel buffer so it stays alive after this callback returns.
        // The consumer (capture/capture_raw/capture_pixel_buffer) will release it.
        CFRetain(pixel_buffer);

        let frame = RetainedPixelBuffer {
            ptr: pixel_buffer,
            width,
            height,
            bytes_per_row,
            timestamp_us: 0, // Set by consumer via capture_pixel_buffer()
        };

        if let Ok(guard) = FRAME_SENDER.lock() {
            if let Some(tx) = guard.as_ref() {
                // try_send: drop frame if receiver is behind.
                // If dropped, RetainedPixelBuffer::drop calls CFRelease.
                let _ = tx.try_send(frame);
            }
        }
    }
}

impl Camera {
    /// Open a camera by index with specified resolution and framerate.
    pub fn open(index: u32, width: u32, height: u32, fps: u32) -> Result<Self> {
        Self::open_with_options(index, width, height, fps, CameraOptions::default())
    }

    /// Open a camera with options.
    pub fn open_with_options(
        index: u32,
        width: u32,
        height: u32,
        _fps: u32,
        _options: CameraOptions,
    ) -> Result<Self> {
        let cameras = list_cameras()?;
        let _cam_info = cameras
            .iter()
            .find(|c| c.index == index)
            .ok_or_else(|| anyhow::anyhow!("Camera index {} not found", index))?;

        unsafe {
            let capture_session = AVCaptureSession::new();
            capture_session.beginConfiguration();

            // Set session preset based on resolution
            let preset_str = match (width, height) {
                (w, h) if w >= 1920 && h >= 1080 => "AVCaptureSessionPreset1920x1080",
                (w, h) if w >= 1280 && h >= 720 => "AVCaptureSessionPreset1280x720",
                _ => "AVCaptureSessionPreset640x480",
            };
            let preset = NSString::from_str(preset_str);
            let can_set: Bool = msg_send![&capture_session, canSetSessionPreset: &*preset];
            if can_set.as_bool() {
                let _: () = msg_send![&capture_session, setSessionPreset: &*preset];
            }

            // Get video device by index
            let media_type = AVMediaTypeVideo.expect("AVMediaTypeVideo not available");

            let device = if index == 0 {
                AVCaptureDevice::defaultDeviceWithMediaType(media_type)
                    .ok_or_else(|| anyhow::anyhow!("No default camera device found"))?
            } else {
                // List all devices and pick by index
                let devices = list_av_devices()?;
                if (index as usize) >= devices.len() {
                    anyhow::bail!(
                        "Camera index {} out of range (found {})",
                        index,
                        devices.len()
                    );
                }
                devices.into_iter().nth(index as usize).unwrap()
            };

            let device_input = AVCaptureDeviceInput::deviceInputWithDevice_error(&device)
                .map_err(|e| anyhow::anyhow!("Failed to create device input: {:?}", e))?;

            if !capture_session.canAddInput(&device_input) {
                anyhow::bail!("Cannot add camera input to session");
            }
            capture_session.addInput(&device_input);

            let video_output = AVCaptureVideoDataOutput::new();

            // Request BGRA pixel format
            let format_key = NSString::from_str("PixelFormatType");
            let format_value: Retained<NSNumber> =
                msg_send![class!(NSNumber), numberWithUnsignedInt: 0x42475241u32]; // 'BGRA'

            let video_settings: Retained<NSObject> = msg_send![
                class!(NSDictionary),
                dictionaryWithObject: &*format_value,
                forKey: &*format_key
            ];
            let _: () = msg_send![&video_output, setVideoSettings: &*video_settings];
            video_output.setAlwaysDiscardsLateVideoFrames(true);

            // Create delegate
            let delegate = create_capture_delegate()?;

            // Create dispatch queue
            let queue_label = b"com.xoq.camera.queue\0";
            let callback_queue =
                dispatch_queue_create(queue_label.as_ptr() as *const i8, std::ptr::null());

            // Set delegate on output
            set_sample_buffer_delegate(
                &*video_output as *const _ as *const std::ffi::c_void,
                &*delegate as *const _ as *const std::ffi::c_void,
                callback_queue,
            );

            if !capture_session.canAddOutput(&video_output) {
                anyhow::bail!("Cannot add video output to session");
            }
            capture_session.addOutput(&video_output);

            capture_session.commitConfiguration();

            // Set up channel before starting
            let (tx, rx) = mpsc::sync_channel(1);
            {
                let mut guard = FRAME_SENDER.lock().unwrap();
                *guard = Some(tx);
            }

            capture_session.startRunning();

            // Determine actual resolution from preset
            let (actual_w, actual_h) = match preset_str {
                "AVCaptureSessionPreset1920x1080" => (1920, 1080),
                "AVCaptureSessionPreset1280x720" => (1280, 720),
                _ => (640, 480),
            };

            Ok(Camera {
                width: actual_w,
                height: actual_h,
                rx,
                start_time: std::time::Instant::now(),
                _session: capture_session,
                _delegate: delegate,
            })
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

    /// Get the capture format name.
    pub fn format_name(&self) -> &'static str {
        "BGRA"
    }

    /// Capture a single frame (converted to RGB).
    ///
    /// Locks the retained CVPixelBuffer, converts BGRA→RGB directly from the
    /// buffer's memory, then releases the buffer. One copy (the conversion) is
    /// unavoidable.
    pub fn capture(&mut self) -> Result<Frame> {
        let retained = self
            .rx
            .recv()
            .map_err(|_| anyhow::anyhow!("Camera capture channel closed"))?;

        let timestamp_us = self.start_time.elapsed().as_micros() as u64;
        let width = retained.width;
        let height = retained.height;
        let bytes_per_row = retained.bytes_per_row;

        let rgb_data = unsafe {
            CVPixelBufferLockBaseAddress(retained.ptr, K_CV_PIXEL_BUFFER_LOCK_READ_ONLY);
            let base = CVPixelBufferGetBaseAddress(retained.ptr);
            let data = if !base.is_null() {
                let bgra = std::slice::from_raw_parts(base, bytes_per_row * height as usize);
                bgra_to_rgb(bgra, width, height, bytes_per_row)
            } else {
                vec![]
            };
            CVPixelBufferUnlockBaseAddress(retained.ptr, K_CV_PIXEL_BUFFER_LOCK_READ_ONLY);
            data
        };
        // retained is dropped here → CFRelease

        Ok(Frame {
            width,
            height,
            data: rgb_data,
            timestamp_us,
        })
    }

    /// Capture a raw frame without conversion (BGRA format).
    ///
    /// Locks the retained CVPixelBuffer, extracts tightly-packed BGRA data
    /// (removing stride padding if any), then releases the buffer.
    pub fn capture_raw(&mut self) -> Result<RawFrame> {
        let retained = self
            .rx
            .recv()
            .map_err(|_| anyhow::anyhow!("Camera capture channel closed"))?;

        let timestamp_us = self.start_time.elapsed().as_micros() as u64;
        let width = retained.width;
        let height = retained.height;
        let bytes_per_row = retained.bytes_per_row;

        let data = unsafe {
            CVPixelBufferLockBaseAddress(retained.ptr, K_CV_PIXEL_BUFFER_LOCK_READ_ONLY);
            let base = CVPixelBufferGetBaseAddress(retained.ptr);
            let d = if !base.is_null() {
                let bgra = std::slice::from_raw_parts(base, bytes_per_row * height as usize);
                extract_packed_bgra(bgra, width, height, bytes_per_row)
            } else {
                vec![]
            };
            CVPixelBufferUnlockBaseAddress(retained.ptr, K_CV_PIXEL_BUFFER_LOCK_READ_ONLY);
            d
        };
        // retained is dropped here → CFRelease

        Ok(RawFrame {
            width,
            height,
            data,
            format: RawFormat::Bgra,
            timestamp_us,
        })
    }

    /// Capture a retained CVPixelBuffer for zero-copy H.264 encoding.
    ///
    /// Returns the CVPixelBuffer directly without any data copies. Pass the
    /// result to `VtEncoder::encode_pixel_buffer()` for zero-copy encoding.
    ///
    /// The buffer is automatically released when the `RetainedPixelBuffer` is dropped.
    pub fn capture_pixel_buffer(&mut self) -> Result<RetainedPixelBuffer> {
        let mut retained = self
            .rx
            .recv()
            .map_err(|_| anyhow::anyhow!("Camera capture channel closed"))?;
        retained.timestamp_us = self.start_time.elapsed().as_micros() as u64;
        Ok(retained)
    }

    /// Check if the camera is capturing in YUYV format.
    /// Always false on macOS (we use BGRA).
    pub fn is_yuyv(&self) -> bool {
        false
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        // Stop capture session
        unsafe { self._session.stopRunning() };
        // Clear the global sender
        if let Ok(mut guard) = FRAME_SENDER.lock() {
            *guard = None;
        }
    }
}

/// Convert BGRA buffer (with possible row padding) to tightly packed RGB.
fn bgra_to_rgb(bgra: &[u8], width: u32, height: u32, bytes_per_row: usize) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let mut rgb = Vec::with_capacity(w * h * 3);

    for y in 0..h {
        let row_start = y * bytes_per_row;
        for x in 0..w {
            let idx = row_start + x * 4;
            if idx + 2 < bgra.len() {
                rgb.push(bgra[idx + 2]); // R
                rgb.push(bgra[idx + 1]); // G
                rgb.push(bgra[idx]); // B
            }
        }
    }

    rgb
}

/// Extract tightly packed BGRA data from a buffer that may have row padding.
fn extract_packed_bgra(bgra: &[u8], width: u32, height: u32, bytes_per_row: usize) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let packed_row = w * 4;

    if bytes_per_row == packed_row {
        // No padding, return as-is (truncated to actual frame size)
        bgra[..packed_row * h].to_vec()
    } else {
        // Remove row padding
        let mut packed = Vec::with_capacity(packed_row * h);
        for y in 0..h {
            let start = y * bytes_per_row;
            let end = start + packed_row;
            if end <= bgra.len() {
                packed.extend_from_slice(&bgra[start..end]);
            }
        }
        packed
    }
}

/// Create an AVCaptureVideoDataOutputSampleBufferDelegate.
unsafe fn create_capture_delegate() -> Result<Retained<NSObject>> {
    use objc2::declare::ClassBuilder;
    use objc2::runtime::AnyProtocol;
    use objc2::ClassType;
    use std::ffi::CStr;

    let class_name = CStr::from_bytes_with_nul(b"XoqCameraMacosDelegate\0").unwrap();
    let protocol_name =
        CStr::from_bytes_with_nul(b"AVCaptureVideoDataOutputSampleBufferDelegate\0").unwrap();

    let protocol =
        AnyProtocol::get(protocol_name).ok_or_else(|| anyhow::anyhow!("Protocol not found"))?;

    let mut builder = ClassBuilder::new(class_name, NSObject::class()).ok_or_else(|| {
        anyhow::anyhow!("Failed to create class builder (class may already exist)")
    })?;
    builder.add_protocol(protocol);
    let delegate_class = builder.register();

    let method_sel = objc2::sel!(captureOutput:didOutputSampleBuffer:fromConnection:);
    let method_types = b"v@:@@@\0";
    let added = class_addMethod(
        delegate_class as *const _ as *const std::ffi::c_void,
        method_sel,
        capture_callback as *const std::ffi::c_void,
        method_types.as_ptr() as *const i8,
    );

    if !added.as_bool() {
        anyhow::bail!("Failed to add method to delegate class");
    }

    let delegate: Retained<NSObject> = msg_send![delegate_class, new];
    Ok(delegate)
}

/// Set sample buffer delegate on an AVCaptureVideoDataOutput.
unsafe fn set_sample_buffer_delegate(
    output: *const std::ffi::c_void,
    delegate: *const std::ffi::c_void,
    queue: *const std::ffi::c_void,
) {
    #[link(name = "objc", kind = "dylib")]
    extern "C" {
        #[link_name = "objc_msgSend"]
        fn objc_msgSend_set_delegate(
            receiver: *const std::ffi::c_void,
            sel: objc2::runtime::Sel,
            delegate: *const std::ffi::c_void,
            queue: *const std::ffi::c_void,
        );
    }

    let sel = objc2::sel!(setSampleBufferDelegate:queue:);
    objc_msgSend_set_delegate(output, sel, delegate, queue);
}

/// List AVCaptureDevices (internal helper).
fn list_av_devices() -> Result<Vec<Retained<AVCaptureDevice>>> {
    let media_type = unsafe { AVMediaTypeVideo.expect("AVMediaTypeVideo not available") };

    // Use discovery session to enumerate all video devices
    let devices: Retained<objc2_foundation::NSArray<AVCaptureDevice>> = unsafe {
        msg_send![
            class!(AVCaptureDevice),
            devicesWithMediaType: media_type
        ]
    };

    let mut result = Vec::new();
    for i in 0..devices.len() {
        result.push(devices.objectAtIndex(i).clone());
    }
    Ok(result)
}

/// List all available cameras.
pub fn list_cameras() -> Result<Vec<CameraInfo>> {
    let devices = list_av_devices()?;

    let cameras: Vec<CameraInfo> = devices
        .iter()
        .enumerate()
        .map(|(i, dev)| {
            let name = unsafe { dev.localizedName() }.to_string();
            let unique_id = unsafe {
                let uid: Retained<NSString> = msg_send![dev, uniqueID];
                uid.to_string()
            };
            CameraInfo {
                index: i as u32,
                name,
                path: PathBuf::from(unique_id),
            }
        })
        .collect();

    Ok(cameras)
}

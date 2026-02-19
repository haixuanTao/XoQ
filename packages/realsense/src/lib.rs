#![allow(clippy::useless_conversion)]
#![allow(non_camel_case_types)]

//! Drop-in replacement for pyrealsense2 — remote RealSense cameras over MoQ.

use numpy::{PyArray1, PyArrayMethods};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use std::sync::{Arc, Mutex};

use xoq::realsense_client::{self, SyncRealSenseClient};

// ============================================================================
// Config
// ============================================================================

#[pyclass]
#[derive(Clone)]
pub struct config {
    device_serial: Option<String>,
    streams: Vec<(i32, i32, i32, i32, i32)>,
}

#[pymethods]
impl config {
    #[new]
    fn new() -> Self {
        Self {
            device_serial: None,
            streams: Vec::new(),
        }
    }

    fn enable_device(&mut self, serial: &str) {
        self.device_serial = Some(serial.to_string());
    }

    #[pyo3(signature = (stream_type, width=0, height=0, format=0, framerate=0))]
    fn enable_stream(
        &mut self,
        stream_type: i32,
        width: i32,
        height: i32,
        format: i32,
        framerate: i32,
    ) {
        self.streams
            .push((stream_type, width, height, format, framerate));
    }
}

// ============================================================================
// Pipeline
// ============================================================================

struct PipelineInner {
    client: SyncRealSenseClient,
    last_intrinsics: Option<realsense_client::Intrinsics>,
}

#[pyclass]
pub struct pipeline {
    inner: Arc<Mutex<Option<PipelineInner>>>,
}

#[pymethods]
impl pipeline {
    #[new]
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
        }
    }

    fn start(&self, cfg: &config) -> PyResult<pipeline_profile> {
        let serial = cfg.device_serial.as_deref().ok_or_else(|| {
            PyRuntimeError::new_err("No device configured. Call config.enable_device() first.")
        })?;

        let client = SyncRealSenseClient::connect_auto(serial)
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to connect: {}", e)))?;

        let mut guard = self
            .inner
            .lock()
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        *guard = Some(PipelineInner {
            client,
            last_intrinsics: None,
        });

        Ok(pipeline_profile {})
    }

    fn wait_for_frames(&self, py: Python<'_>) -> PyResult<frameset> {
        let mut pipeline_inner = {
            let mut guard = self
                .inner
                .lock()
                .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
            guard.take()
        };

        let result = if let Some(ref mut pi) = pipeline_inner {
            let res = py.allow_threads(|| pi.client.read_frames());
            if let Some(intr) = pi.client.intrinsics() {
                pi.last_intrinsics = Some(intr);
            }
            match res {
                Ok(frames) => Ok((frames, pi.last_intrinsics)),
                Err(e) => Err(PyRuntimeError::new_err(format!(
                    "read_frames failed: {}",
                    e
                ))),
            }
        } else {
            Err(PyRuntimeError::new_err("Pipeline not started"))
        };

        // Put client back
        if let Some(pi) = pipeline_inner {
            let mut guard = self
                .inner
                .lock()
                .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
            *guard = Some(pi);
        }

        let (frames, intr_opt) = result?;

        let intr = intr_opt.map(|i| intrinsics {
            width: i.width as i32,
            height: i.height as i32,
            fx: i.fx,
            fy: i.fy,
            ppx: i.ppx,
            ppy: i.ppy,
        });

        Ok(frameset {
            color_rgb: frames.color_rgb,
            depth_mm: frames.depth_mm,
            width: frames.width,
            height: frames.height,
            intrinsics: intr,
        })
    }

    fn stop(&self) -> PyResult<()> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| PyRuntimeError::new_err(format!("{}", e)))?;
        *guard = None;
        Ok(())
    }
}

// ============================================================================
// Pipeline Profile (stub)
// ============================================================================

#[pyclass]
pub struct pipeline_profile {}

// ============================================================================
// Frameset
// ============================================================================

#[pyclass]
pub struct frameset {
    color_rgb: Vec<u8>,
    depth_mm: Vec<u16>,
    width: u32,
    height: u32,
    intrinsics: Option<intrinsics>,
}

#[pymethods]
impl frameset {
    fn get_depth_frame(&self) -> depth_frame {
        depth_frame {
            data: self.depth_mm.clone(),
            width: self.width,
            height: self.height,
            intrinsics: self.intrinsics.clone(),
        }
    }

    fn get_color_frame(&self) -> color_frame {
        color_frame {
            data: self.color_rgb.clone(),
            width: self.width,
            height: self.height,
            intrinsics: self.intrinsics.clone(),
        }
    }
}

// ============================================================================
// Depth Frame
// ============================================================================

#[pyclass]
pub struct depth_frame {
    data: Vec<u16>,
    width: u32,
    height: u32,
    intrinsics: Option<intrinsics>,
}

#[pymethods]
impl depth_frame {
    fn get_data<'py>(&self, py: Python<'py>) -> PyResult<PyObject> {
        let array = PyArray1::from_vec_bound(py, self.data.clone());
        let reshaped = array
            .reshape([self.height as usize, self.width as usize])
            .map_err(|e: PyErr| e)?;
        Ok(reshaped.into_any().unbind())
    }

    fn get_distance(&self, x: i32, y: i32) -> f64 {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return 0.0;
        }
        let idx = (y as usize) * (self.width as usize) + (x as usize);
        self.data.get(idx).copied().unwrap_or(0) as f64 / 1000.0
    }

    #[getter]
    fn profile(&self) -> video_stream_profile {
        video_stream_profile {
            intrinsics: self.intrinsics.clone(),
            stream_type: STREAM_DEPTH,
            width: self.width as i32,
            height: self.height as i32,
        }
    }

    fn get_width(&self) -> i32 {
        self.width as i32
    }

    fn get_height(&self) -> i32 {
        self.height as i32
    }

    fn __bool__(&self) -> bool {
        !self.data.is_empty()
    }
}

// ============================================================================
// Color Frame
// ============================================================================

#[pyclass]
pub struct color_frame {
    data: Vec<u8>,
    width: u32,
    height: u32,
    intrinsics: Option<intrinsics>,
}

#[pymethods]
impl color_frame {
    fn get_data<'py>(&self, py: Python<'py>) -> PyResult<PyObject> {
        let array = PyArray1::from_vec_bound(py, self.data.clone());
        let reshaped = array
            .reshape([self.height as usize, self.width as usize, 3])
            .map_err(|e: PyErr| e)?;
        Ok(reshaped.into_any().unbind())
    }

    #[getter]
    fn profile(&self) -> video_stream_profile {
        video_stream_profile {
            intrinsics: self.intrinsics.clone(),
            stream_type: STREAM_COLOR,
            width: self.width as i32,
            height: self.height as i32,
        }
    }

    fn get_width(&self) -> i32 {
        self.width as i32
    }

    fn get_height(&self) -> i32 {
        self.height as i32
    }

    fn __bool__(&self) -> bool {
        !self.data.is_empty()
    }
}

// ============================================================================
// Video Stream Profile
// ============================================================================

#[pyclass]
#[derive(Clone)]
pub struct video_stream_profile {
    intrinsics: Option<intrinsics>,
    stream_type: i32,
    width: i32,
    height: i32,
}

#[pymethods]
impl video_stream_profile {
    fn as_video_stream_profile(&self) -> video_stream_profile {
        self.clone()
    }

    fn get_intrinsics(&self) -> intrinsics {
        self.intrinsics.clone().unwrap_or(intrinsics {
            width: self.width,
            height: self.height,
            fx: self.width as f32 * 0.6,
            fy: self.height as f32 * 0.8,
            ppx: self.width as f32 / 2.0,
            ppy: self.height as f32 / 2.0,
        })
    }

    fn stream_type(&self) -> i32 {
        self.stream_type
    }
}

// ============================================================================
// Intrinsics
// ============================================================================

#[pyclass]
#[derive(Clone)]
pub struct intrinsics {
    #[pyo3(get)]
    pub width: i32,
    #[pyo3(get)]
    pub height: i32,
    #[pyo3(get)]
    pub fx: f32,
    #[pyo3(get)]
    pub fy: f32,
    #[pyo3(get)]
    pub ppx: f32,
    #[pyo3(get)]
    pub ppy: f32,
}

#[pymethods]
impl intrinsics {
    fn __repr__(&self) -> String {
        format!(
            "intrinsics: [ {}x{} p[{:.4} {:.4}] f[{:.4} {:.4}] ]",
            self.width, self.height, self.ppx, self.ppy, self.fx, self.fy
        )
    }
}

// ============================================================================
// Align (passthrough for remote — server already aligns)
// ============================================================================

#[pyclass]
pub struct align {
    _align_to: i32,
}

#[pymethods]
impl align {
    #[new]
    fn new(align_to: i32) -> Self {
        Self {
            _align_to: align_to,
        }
    }

    fn process(&self, frames: &frameset) -> frameset {
        frameset {
            color_rgb: frames.color_rgb.clone(),
            depth_mm: frames.depth_mm.clone(),
            width: frames.width,
            height: frames.height,
            intrinsics: frames.intrinsics.clone(),
        }
    }
}

// ============================================================================
// Stream / Format constants
// ============================================================================

const STREAM_COLOR: i32 = 1;
const STREAM_DEPTH: i32 = 2;
const STREAM_INFRARED: i32 = 3;

#[pyclass]
pub struct stream {}

#[pymethods]
impl stream {
    #[classattr]
    fn color() -> i32 {
        STREAM_COLOR
    }
    #[classattr]
    fn depth() -> i32 {
        STREAM_DEPTH
    }
    #[classattr]
    fn infrared() -> i32 {
        STREAM_INFRARED
    }
}

#[pyclass]
pub struct format {}

#[pymethods]
impl format {
    #[classattr]
    fn rgb8() -> i32 {
        1
    }
    #[classattr]
    fn z16() -> i32 {
        2
    }
    #[classattr]
    fn bgr8() -> i32 {
        3
    }
    #[classattr]
    fn any() -> i32 {
        0
    }
}

// ============================================================================
// Module definition
// ============================================================================

#[pymodule]
fn xoq_realsense(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<config>()?;
    m.add_class::<pipeline>()?;
    m.add_class::<pipeline_profile>()?;
    m.add_class::<frameset>()?;
    m.add_class::<depth_frame>()?;
    m.add_class::<color_frame>()?;
    m.add_class::<video_stream_profile>()?;
    m.add_class::<intrinsics>()?;
    m.add_class::<align>()?;
    m.add_class::<stream>()?;
    m.add_class::<format>()?;
    Ok(())
}

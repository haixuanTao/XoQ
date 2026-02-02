//! Sync camera client for Python bindings.
//!
//! This module provides a blocking API for remote cameras,
//! managing its own tokio runtime internally.
//! Wraps the async `CameraClient` to get H.264 negotiation and
//! decoding (NVDEC or VideoToolbox) for free.

use anyhow::Result;

use crate::frame::Frame;
use crate::opencv::{CameraClient, CameraClientBuilder};

/// A synchronous client for remote cameras.
///
/// This client manages its own tokio runtime internally,
/// providing a simple blocking API. It wraps the async `CameraClient`,
/// automatically supporting H.264 decoding when the appropriate
/// feature (nvenc or videotoolbox) is enabled.
pub struct SyncCameraClient {
    inner: CameraClient,
    runtime: tokio::runtime::Runtime,
}

impl SyncCameraClient {
    /// Connect to a remote camera server via iroh P2P.
    pub fn connect(server_id: &str) -> Result<Self> {
        let runtime = tokio::runtime::Runtime::new()?;
        let client = runtime.block_on(CameraClient::connect(server_id))?;
        Ok(Self {
            inner: client,
            runtime,
        })
    }

    /// Connect to a remote camera server via MoQ relay.
    ///
    /// Auto-negotiates H.264 when the `videotoolbox` or `nvenc` feature is enabled;
    /// falls back to JPEG otherwise.
    pub fn connect_moq(path: &str) -> Result<Self> {
        let runtime = tokio::runtime::Runtime::new()?;
        let client = runtime.block_on(
            CameraClientBuilder::new().moq(path).connect(),
        )?;
        Ok(Self {
            inner: client,
            runtime,
        })
    }

    /// Auto-detect transport and connect.
    ///
    /// Uses MoQ if the source contains `/` (e.g. `anon/camera-0`),
    /// otherwise treats it as an iroh server ID.
    pub fn connect_auto(source: &str) -> Result<Self> {
        if source.contains('/') {
            Self::connect_moq(source)
        } else {
            Self::connect(source)
        }
    }

    /// Read a frame from the remote camera.
    pub fn read_frame(&mut self) -> Result<Frame> {
        self.runtime.block_on(self.inner.read_frame())
    }
}

//! Sync serial client for Python bindings.
//!
//! This module provides a blocking API for remote serial ports,
//! managing its own tokio runtime internally.

use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use crate::iroh::IrohClientBuilder;

/// A synchronous client for remote serial ports.
///
/// This client manages its own tokio runtime internally,
/// providing a simple blocking API.
pub struct SyncSerialClient {
    send: Arc<Mutex<iroh::endpoint::SendStream>>,
    recv: Arc<Mutex<iroh::endpoint::RecvStream>>,
    runtime: tokio::runtime::Runtime,
    _conn: crate::iroh::IrohConnection,
}

impl SyncSerialClient {
    /// Connect to a remote serial port server.
    pub fn connect(server_id: &str) -> Result<Self> {
        let runtime = tokio::runtime::Runtime::new()?;

        let (send, recv, conn) = runtime.block_on(async {
            let conn = IrohClientBuilder::new().connect_str(server_id).await?;
            let stream = conn.open_stream().await?;
            let (send, recv) = stream.split();
            Ok::<_, anyhow::Error>((send, recv, conn))
        })?;

        Ok(Self {
            send: Arc::new(Mutex::new(send)),
            recv: Arc::new(Mutex::new(recv)),
            runtime,
            _conn: conn,
        })
    }

    /// Write data to the remote serial port.
    pub fn write(&self, data: &[u8]) -> Result<()> {
        self.runtime.block_on(async {
            let mut send = self.send.lock().await;
            send.write_all(data).await?;
            drop(send);
            // quinn's flush() is a no-op — yield to let connection task send
            tokio::time::sleep(std::time::Duration::from_micros(100)).await;
            Ok(())
        })
    }

    /// Read data from the remote serial port.
    pub fn read(&self, buf: &mut [u8]) -> Result<Option<usize>> {
        self.runtime.block_on(async {
            let mut recv = self.recv.lock().await;
            Ok(recv.read(buf).await?)
        })
    }

    /// Read with timeout.
    pub fn read_timeout(&self, buf: &mut [u8], timeout: Duration) -> Result<Option<usize>> {
        self.runtime.block_on(async {
            match tokio::time::timeout(timeout, async {
                let mut recv = self.recv.lock().await;
                recv.read(buf).await
            })
            .await
            {
                Ok(Ok(n)) => Ok(n),
                Ok(Err(e)) => Err(e.into()),
                Err(_) => Ok(None), // Timeout
            }
        })
    }
}

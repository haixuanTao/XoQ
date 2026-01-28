//! Drop-in replacement for pyserial - remote serial ports over P2P.
//!
//! This module provides a `serial.Serial` compatible class that connects
//! to remote serial ports over iroh P2P.
//!
//! # Example
//!
//! ```python
//! import serial
//!
//! # Connect to a remote serial port
//! ser = serial.Serial('server-endpoint-id', timeout=1.0)
//! ser.write(b'Hello')
//! data = ser.readline()
//! ```

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use std::sync::Arc;

/// Run an async function in a dedicated thread with its own runtime.
/// This avoids "Cannot start a runtime from within a runtime" errors
/// when called from environments that already have an event loop (e.g., Jupyter).
fn run_async<F, T>(f: F) -> PyResult<T>
where
    F: std::future::Future<Output = Result<T, anyhow::Error>> + Send + 'static,
    T: Send + 'static,
{
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to create runtime: {}", e)))?;
        rt.block_on(f)
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))
    })
    .join()
    .map_err(|_| PyRuntimeError::new_err("Thread panicked"))?
}

/// Find a subsequence in a slice, returns the starting position if found.
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// A pyserial-compatible interface to a remote serial port.
/// Drop-in replacement for serial.Serial that connects over iroh P2P.
///
/// Example:
///     ser = serial.Serial('abc123...')  # server endpoint id
///     ser.write(b'AT\r\n')
///     response = ser.readline()
#[pyclass]
pub struct Serial {
    inner: Arc<xoq::Client>,
    buffer: Arc<std::sync::Mutex<Vec<u8>>>,
    is_open: Arc<std::sync::atomic::AtomicBool>,
    timeout: Option<f64>,
    port_name: String,
}

#[pymethods]
impl Serial {
    /// Open a connection to a remote serial port.
    ///
    /// Args:
    ///     port: The server's endpoint ID (equivalent to port name in pyserial)
    ///     timeout: Read timeout in seconds (None for blocking)
    #[new]
    #[pyo3(signature = (port, timeout=None))]
    fn new(port: &str, timeout: Option<f64>) -> PyResult<Self> {
        let port_name = port.to_string();
        runtime().block_on(async {
            let client = xoq::Client::connect(port)
                .await
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

            Ok(Serial {
                inner: Arc::new(client),
                buffer: Arc::new(std::sync::Mutex::new(Vec::new())),
                is_open: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                timeout,
                port_name,
            })
        })
    }

    /// Write bytes to the serial port. Returns number of bytes written.
    fn write(&self, data: Vec<u8>) -> PyResult<usize> {
        if !self.is_open.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(PyRuntimeError::new_err("Port is closed"));
        }
        let len = data.len();
        runtime().block_on(async {
            self.inner
                .write(&data)
                .await
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            Ok(len)
        })
    }

    /// Read up to size bytes from the serial port.
    #[pyo3(signature = (size=1))]
    fn read(&self, size: usize) -> PyResult<Vec<u8>> {
        if !self.is_open.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(PyRuntimeError::new_err("Port is closed"));
        }

        // First check buffer
        {
            let mut buf = self.buffer.lock().unwrap();
            if !buf.is_empty() {
                let take = std::cmp::min(size, buf.len());
                let result: Vec<u8> = buf.drain(..take).collect();
                return Ok(result);
            }
        }

        // Read from network
        runtime().block_on(async {
            let mut data = vec![0u8; size];
            let n = self
                .inner
                .read(&mut data)
                .await
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

            match n {
                Some(n) => Ok(data[..n].to_vec()),
                None => Ok(Vec::new()),
            }
        })
    }

    /// Read a line (until newline character).
    fn readline(&self) -> PyResult<Vec<u8>> {
        if !self.is_open.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(PyRuntimeError::new_err("Port is closed"));
        }

        let mut result = Vec::new();

        // Check buffer first for existing newline
        {
            let mut buf = self.buffer.lock().unwrap();
            if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                result = buf.drain(..=pos).collect();
                return Ok(result);
            }
            // Take everything from buffer
            result.append(&mut *buf);
        }

        // Keep reading until we get a newline
        runtime().block_on(async {
            let mut temp = vec![0u8; 256];
            loop {
                let n = self
                    .inner
                    .read(&mut temp)
                    .await
                    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

                match n {
                    Some(n) => {
                        let chunk = &temp[..n];
                        if let Some(pos) = chunk.iter().position(|&b| b == b'\n') {
                            // Found newline - take up to and including it
                            result.extend_from_slice(&chunk[..=pos]);
                            // Buffer the rest
                            if pos + 1 < n {
                                let mut buf = self.buffer.lock().unwrap();
                                buf.extend_from_slice(&chunk[pos + 1..]);
                            }
                            return Ok(result);
                        } else {
                            result.extend_from_slice(chunk);
                        }
                    }
                    None => return Ok(result), // EOF
                }
            }
        })
    }

    /// Number of bytes in the receive buffer.
    #[getter]
    fn in_waiting(&self) -> usize {
        self.buffer.lock().unwrap().len()
    }

    /// Whether the port is open.
    #[getter]
    fn is_open(&self) -> bool {
        self.is_open.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Read/write timeout in seconds.
    #[getter]
    fn timeout(&self) -> Option<f64> {
        self.timeout
    }

    /// The port name (server endpoint ID).
    #[getter]
    fn port(&self) -> &str {
        &self.port_name
    }

    /// Alias for port property (pyserial compatibility).
    #[getter]
    fn name(&self) -> &str {
        &self.port_name
    }

    /// Clear the receive buffer.
    fn reset_input_buffer(&self) {
        self.buffer.lock().unwrap().clear();
    }

    /// Read until a terminator sequence is found.
    #[pyo3(signature = (terminator=None))]
    fn read_until(&self, terminator: Option<Vec<u8>>) -> PyResult<Vec<u8>> {
        let terminator = terminator.unwrap_or_else(|| vec![b'\n']);
        if terminator.is_empty() {
            return Err(PyRuntimeError::new_err("Terminator cannot be empty"));
        }

        if !self.is_open.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(PyRuntimeError::new_err("Port is closed"));
        }

        let mut result = Vec::new();

        // Check buffer first for existing terminator
        {
            let mut buf = self.buffer.lock().unwrap();
            if let Some(pos) = find_subsequence(&buf, &terminator) {
                let end = pos + terminator.len();
                result = buf.drain(..end).collect();
                return Ok(result);
            }
            // Take everything from buffer
            result.append(&mut *buf);
        }

        // Keep reading until we find terminator
        runtime().block_on(async {
            let mut temp = vec![0u8; 256];
            loop {
                let n = self
                    .inner
                    .read(&mut temp)
                    .await
                    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

                match n {
                    Some(n) => {
                        result.extend_from_slice(&temp[..n]);
                        // Check if terminator is now in result
                        if let Some(pos) = find_subsequence(&result, &terminator) {
                            let end = pos + terminator.len();
                            // Buffer anything after the terminator
                            if end < result.len() {
                                let mut buf = self.buffer.lock().unwrap();
                                buf.extend_from_slice(&result[end..]);
                            }
                            result.truncate(end);
                            return Ok(result);
                        }
                    }
                    None => return Ok(result), // EOF
                }
            }
        })
    }

    /// Flush write buffer (no-op for network connection).
    fn flush(&self) -> PyResult<()> {
        Ok(())
    }

    /// Close the connection.
    fn close(&self) -> PyResult<()> {
        self.is_open
            .store(false, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// Context manager enter.
    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    /// Context manager exit.
    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __exit__(
        &self,
        _exc_type: Option<&pyo3::Bound<'_, pyo3::types::PyAny>>,
        _exc_val: Option<&pyo3::Bound<'_, pyo3::types::PyAny>>,
        _exc_tb: Option<&pyo3::Bound<'_, pyo3::types::PyAny>>,
    ) -> PyResult<bool> {
        self.close()?;
        Ok(false)
    }
}

// pyserial constants
#[pymodule]
fn serial(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Serial>()?;

    // Parity constants
    m.add("PARITY_NONE", "N")?;
    m.add("PARITY_EVEN", "E")?;
    m.add("PARITY_ODD", "O")?;
    m.add("PARITY_MARK", "M")?;
    m.add("PARITY_SPACE", "S")?;

    // Stop bits constants
    m.add("STOPBITS_ONE", 1.0)?;
    m.add("STOPBITS_ONE_POINT_FIVE", 1.5)?;
    m.add("STOPBITS_TWO", 2.0)?;

    // Byte size constants
    m.add("FIVEBITS", 5)?;
    m.add("SIXBITS", 6)?;
    m.add("SEVENBITS", 7)?;
    m.add("EIGHTBITS", 8)?;

    Ok(())
}

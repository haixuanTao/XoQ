//! Serial port bridge over iroh P2P
//!
//! Handles all the forwarding logic internally so users just need to
//! start the bridge and it handles everything.
//!
//! # Remote Serial Port
//!
//! The [`RemoteSerialPort`] struct provides a `serialport`-compatible API for
//! connecting to remote serial ports over iroh P2P. It implements `std::io::Read`
//! and `std::io::Write` traits for seamless integration with existing code.
//!
//! ```no_run
//! use wser::RemoteSerialPort;
//! use std::io::{Read, Write};
//!
//! let mut port = RemoteSerialPort::open("server-endpoint-id")?;
//! port.write_all(b"AT\r\n")?;
//! let mut buf = [0u8; 100];
//! let n = port.read(&mut buf)?;
//! ```

use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use crate::iroh::{IrohClientBuilder, IrohServerBuilder, IrohConnection};
use crate::serial::{SerialPort, SerialReader, SerialWriter};

/// A server that bridges a local serial port to remote clients over iroh P2P
pub struct Server {
    server_id: String,
    serial_reader: Arc<Mutex<SerialReader>>,
    serial_writer: Arc<Mutex<SerialWriter>>,
    endpoint: Arc<crate::iroh::IrohServer>,
}

impl Server {
    /// Create a new serial bridge server
    ///
    /// Args:
    ///     port: Serial port name (e.g., "/dev/ttyUSB0" or "COM3")
    ///     baud_rate: Baud rate (e.g., 115200)
    ///     identity_path: Optional path to save/load server identity
    pub async fn new(
        port: &str,
        baud_rate: u32,
        identity_path: Option<&str>,
    ) -> Result<Self> {
        // Open serial port
        let serial = SerialPort::open_simple(port, baud_rate)?;
        let (reader, writer) = serial.split();

        // Start iroh server
        let mut builder = IrohServerBuilder::new();
        if let Some(path) = identity_path {
            builder = builder.identity_path(path);
        }
        let server = builder.bind().await?;
        let server_id = server.id().to_string();

        Ok(Self {
            server_id,
            serial_reader: Arc::new(Mutex::new(reader)),
            serial_writer: Arc::new(Mutex::new(writer)),
            endpoint: Arc::new(server),
        })
    }

    /// Get the server's endpoint ID (share this with clients)
    pub fn id(&self) -> &str {
        &self.server_id
    }

    /// Run the bridge server (blocks forever, handling connections)
    pub async fn run(&self) -> Result<()> {
        tracing::info!("Serial bridge server running. ID: {}", self.server_id);

        loop {
            // Accept connection
            let conn = match self.endpoint.accept().await? {
                Some(c) => c,
                None => continue,
            };

            tracing::info!("Client connected: {}", conn.remote_id());

            // Handle this connection
            if let Err(e) = self.handle_connection(conn).await {
                tracing::error!("Connection error: {}", e);
            }

            tracing::info!("Client disconnected");
        }
    }

    /// Run the bridge server for a single connection, then return
    pub async fn run_once(&self) -> Result<()> {
        tracing::info!("Serial bridge server waiting for connection. ID: {}", self.server_id);

        loop {
            let conn = match self.endpoint.accept().await? {
                Some(c) => c,
                None => continue,
            };

            tracing::info!("Client connected: {}", conn.remote_id());

            if let Err(e) = self.handle_connection(conn).await {
                tracing::error!("Connection error: {}", e);
            }

            tracing::info!("Client disconnected");
            return Ok(());
        }
    }

    async fn handle_connection(&self, conn: IrohConnection) -> Result<()> {
        let stream = conn.accept_stream().await?;
        let stream = Arc::new(Mutex::new(stream));

        let serial_reader = self.serial_reader.clone();
        let serial_writer = self.serial_writer.clone();
        let stream_clone = stream.clone();

        // Spawn task: serial -> network
        let serial_to_net = tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            loop {
                let n = {
                    let mut reader = serial_reader.lock().await;
                    match reader.read(&mut buf).await {
                        Ok(n) if n > 0 => n,
                        Ok(_) => break,
                        Err(e) => {
                            tracing::debug!("Serial read error: {}", e);
                            break;
                        }
                    }
                };

                let mut s = stream_clone.lock().await;
                if s.write(&buf[..n]).await.is_err() {
                    break;
                }
            }
        });

        // Main task: network -> serial
        let mut buf = vec![0u8; 1024];
        loop {
            let data = {
                let mut s = stream.lock().await;
                match s.read(&mut buf).await {
                    Ok(Some(n)) if n > 0 => buf[..n].to_vec(),
                    Ok(_) => break,
                    Err(_) => break,
                }
            };

            let mut writer = serial_writer.lock().await;
            if writer.write_all(&data).await.is_err() {
                break;
            }
        }

        serial_to_net.abort();
        Ok(())
    }
}

/// A client that connects to a remote serial port over iroh P2P
pub struct Client {
    stream: Arc<Mutex<crate::iroh::IrohStream>>,
    _conn: IrohConnection,
}

impl Client {
    /// Connect to a remote serial bridge server
    pub async fn connect(server_id: &str) -> Result<Self> {
        let conn = IrohClientBuilder::new().connect_str(server_id).await?;
        let stream = conn.open_stream().await?;

        Ok(Self {
            stream: Arc::new(Mutex::new(stream)),
            _conn: conn,
        })
    }

    /// Write data to the remote serial port
    pub async fn write(&self, data: &[u8]) -> Result<()> {
        let mut stream = self.stream.lock().await;
        stream.write(data).await
    }

    /// Write a string to the remote serial port
    pub async fn write_str(&self, data: &str) -> Result<()> {
        let mut stream = self.stream.lock().await;
        stream.write_str(data).await
    }

    /// Read data from the remote serial port
    pub async fn read(&self, buf: &mut [u8]) -> Result<Option<usize>> {
        let mut stream = self.stream.lock().await;
        stream.read(buf).await
    }

    /// Read a string from the remote serial port
    pub async fn read_string(&self) -> Result<Option<String>> {
        let mut stream = self.stream.lock().await;
        stream.read_string().await
    }

    /// Run an interactive bridge to local stdin/stdout
    pub async fn run_interactive(&self) -> Result<()> {
        use std::io::{Read, Write};

        let stream = self.stream.clone();

        // Spawn task: network -> stdout
        let stream_clone = stream.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 1024];
            loop {
                let data = {
                    let mut s = stream_clone.lock().await;
                    match s.read(&mut buf).await {
                        Ok(Some(n)) if n > 0 => buf[..n].to_vec(),
                        _ => break,
                    }
                };
                let _ = std::io::stdout().write_all(&data);
                let _ = std::io::stdout().flush();
            }
        });

        // Main: stdin -> network
        loop {
            let result = tokio::task::spawn_blocking(|| {
                let mut buf = [0u8; 256];
                match std::io::stdin().read(&mut buf) {
                    Ok(n) if n > 0 => Some(buf[..n].to_vec()),
                    _ => None,
                }
            })
            .await?;

            match result {
                Some(data) => {
                    let mut s = stream.lock().await;
                    if s.write(&data).await.is_err() {
                        break;
                    }
                }
                None => break,
            }
        }

        Ok(())
    }
}

/// Transport type for the serial port connection.
#[derive(Clone)]
pub enum Transport {
    /// Iroh P2P connection (default)
    Iroh {
        /// Custom ALPN protocol
        alpn: Option<Vec<u8>>,
    },
    /// MoQ relay connection
    Moq {
        /// Relay URL
        relay: String,
        /// Authentication token
        token: Option<String>,
    },
}

impl Default for Transport {
    fn default() -> Self {
        Transport::Iroh { alpn: None }
    }
}

/// Builder for creating a remote serial port connection.
///
/// Mimics the `serialport::new()` API for drop-in compatibility.
///
/// # Example
///
/// ```no_run
/// use wser::serialport;
/// use std::time::Duration;
///
/// // Simple iroh P2P connection (default)
/// let port = serialport::new("server-endpoint-id").open()?;
///
/// // With timeout
/// let port = serialport::new("server-endpoint-id")
///     .timeout(Duration::from_millis(500))
///     .open()?;
///
/// // With MoQ relay
/// let port = serialport::new("my-channel")
///     .with_moq("https://relay.example.com")
///     .token("jwt-token")
///     .open()?;
/// # Ok::<(), anyhow::Error>(())
/// ```
pub struct SerialPortBuilder {
    port_name: String,
    timeout: Duration,
    transport: Transport,
}

impl SerialPortBuilder {
    /// Create a new serial port builder.
    pub fn new(port: &str) -> Self {
        Self {
            port_name: port.to_string(),
            timeout: Duration::from_secs(1),
            transport: Transport::default(),
        }
    }

    /// Set the read/write timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Use iroh P2P transport (default).
    pub fn with_iroh(mut self) -> Self {
        self.transport = Transport::Iroh { alpn: None };
        self
    }

    /// Set custom ALPN for iroh connection.
    pub fn alpn(mut self, alpn: &[u8]) -> Self {
        if let Transport::Iroh { alpn: ref mut a } = self.transport {
            *a = Some(alpn.to_vec());
        }
        self
    }

    /// Use MoQ relay transport.
    pub fn with_moq(mut self, relay: &str) -> Self {
        self.transport = Transport::Moq {
            relay: relay.to_string(),
            token: None,
        };
        self
    }

    /// Set authentication token (for MoQ).
    pub fn token(mut self, token: &str) -> Self {
        if let Transport::Moq { token: ref mut t, .. } = self.transport {
            *t = Some(token.to_string());
        }
        self
    }

    /// Open the connection to the remote serial port.
    pub fn open(self) -> Result<RemoteSerialPort> {
        let runtime = tokio::runtime::Runtime::new()?;

        let client = match self.transport {
            Transport::Iroh { alpn } => {
                runtime.block_on(async {
                    let mut builder = IrohClientBuilder::new();
                    if let Some(alpn) = alpn {
                        builder = builder.alpn(&alpn);
                    }
                    let conn = builder.connect_str(&self.port_name).await?;
                    let stream = conn.open_stream().await?;
                    Ok::<_, anyhow::Error>(ClientInner::Iroh {
                        stream: Arc::new(Mutex::new(stream)),
                        _conn: conn,
                    })
                })?
            }
            Transport::Moq { relay, token } => {
                runtime.block_on(async {
                    let mut builder = crate::moq::MoqBuilder::new()
                        .relay(&relay)
                        .path(&self.port_name);
                    if let Some(t) = token {
                        builder = builder.token(&t);
                    }
                    let conn = builder.connect_duplex().await?;
                    Ok::<_, anyhow::Error>(ClientInner::Moq {
                        conn: Arc::new(tokio::sync::Mutex::new(conn)),
                    })
                })?
            }
        };

        Ok(RemoteSerialPort {
            client,
            runtime,
            port_name: self.port_name,
            timeout: self.timeout,
            buffer: Vec::new(),
        })
    }
}

/// Internal client representation supporting multiple transports.
enum ClientInner {
    Iroh {
        stream: Arc<Mutex<crate::iroh::IrohStream>>,
        _conn: IrohConnection,
    },
    Moq {
        conn: Arc<tokio::sync::Mutex<crate::moq::MoqConnection>>,
    },
}

/// Create a new remote serial port builder.
///
/// This function mimics `serialport::new()` for drop-in compatibility.
///
/// # Example
///
/// ```no_run
/// use wser::serialport;
///
/// // Drop-in replacement for serialport crate
/// let port = serialport::new("server-endpoint-id").open()?;
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn new(port: &str) -> SerialPortBuilder {
    SerialPortBuilder::new(port)
}

/// A `serialport`-compatible interface to a remote serial port.
///
/// This struct provides a blocking API that mimics the `serialport` crate,
/// implementing `std::io::Read` and `std::io::Write` traits for seamless
/// integration with existing code. Supports both iroh P2P and MoQ relay transports.
///
/// # Example
///
/// ```no_run
/// use wser::serialport;
/// use std::io::{BufRead, BufReader, Write};
///
/// // Iroh P2P (default)
/// let mut port = serialport::new("server-endpoint-id").open()?;
///
/// // Or with MoQ relay
/// let mut port = serialport::new("my-channel")
///     .with_moq("https://relay.example.com")
///     .open()?;
///
/// port.write_all(b"AT\r\n")?;
/// let mut reader = BufReader::new(port);
/// let mut line = String::new();
/// reader.read_line(&mut line)?;
/// # Ok::<(), anyhow::Error>(())
/// ```
pub struct RemoteSerialPort {
    client: ClientInner,
    runtime: tokio::runtime::Runtime,
    port_name: String,
    timeout: Duration,
    buffer: Vec<u8>,
}

impl RemoteSerialPort {
    /// Open a connection to a remote serial port via iroh P2P.
    ///
    /// Prefer using `wser::serialport::new(port).open()` for more options.
    pub fn open(port: &str) -> Result<Self> {
        new(port).open()
    }

    /// Get the port name (server endpoint ID or MoQ path).
    pub fn name(&self) -> Option<String> {
        Some(self.port_name.clone())
    }

    /// Get the current timeout.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Set the read/write timeout.
    pub fn set_timeout(&mut self, timeout: Duration) -> Result<()> {
        self.timeout = timeout;
        Ok(())
    }

    /// Get the number of bytes available to read.
    pub fn bytes_to_read(&self) -> Result<u32> {
        Ok(self.buffer.len() as u32)
    }

    /// Get the number of bytes waiting to be written (always 0 for network).
    pub fn bytes_to_write(&self) -> Result<u32> {
        Ok(0)
    }

    /// Clear the input buffer.
    pub fn clear_input(&mut self) -> Result<()> {
        self.buffer.clear();
        Ok(())
    }

    /// Clear the output buffer (no-op for network).
    pub fn clear_output(&mut self) -> Result<()> {
        Ok(())
    }

    /// Clear all buffers.
    pub fn clear_all(&mut self) -> Result<()> {
        self.buffer.clear();
        Ok(())
    }

    /// Write bytes to the remote serial port.
    pub fn write_bytes(&mut self, data: &[u8]) -> Result<usize> {
        self.runtime.block_on(async {
            match &self.client {
                ClientInner::Iroh { stream, .. } => {
                    let mut s = stream.lock().await;
                    s.write(data).await?;
                }
                ClientInner::Moq { conn } => {
                    let mut c = conn.lock().await;
                    let mut track = c.create_track("serial");
                    track.write(data.to_vec());
                }
            }
            Ok::<_, anyhow::Error>(())
        })?;
        Ok(data.len())
    }

    /// Read bytes from the remote serial port.
    pub fn read_bytes(&mut self, buf: &mut [u8]) -> Result<usize> {
        // First drain from buffer
        if !self.buffer.is_empty() {
            let take = std::cmp::min(buf.len(), self.buffer.len());
            buf[..take].copy_from_slice(&self.buffer[..take]);
            self.buffer.drain(..take);
            return Ok(take);
        }

        // Read from network with timeout
        let timeout = self.timeout;
        let result = self.runtime.block_on(async {
            tokio::time::timeout(timeout, async {
                match &self.client {
                    ClientInner::Iroh { stream, .. } => {
                        let mut s = stream.lock().await;
                        s.read(buf).await
                    }
                    ClientInner::Moq { conn } => {
                        let mut c = conn.lock().await;
                        if let Some(reader) = c.subscribe_track("serial").await? {
                            let mut reader = reader;
                            if let Some(data) = reader.read().await? {
                                let n = std::cmp::min(data.len(), buf.len());
                                buf[..n].copy_from_slice(&data[..n]);
                                Ok(Some(n))
                            } else {
                                Ok(None)
                            }
                        } else {
                            Ok(None)
                        }
                    }
                }
            }).await
        });

        match result {
            Ok(Ok(Some(n))) => Ok(n),
            Ok(Ok(None)) => Ok(0),
            Ok(Err(e)) => Err(e),
            Err(_) => Ok(0), // Timeout
        }
    }

    /// Read until a specific byte is found.
    pub fn read_until(&mut self, byte: u8) -> Result<Vec<u8>> {
        let mut result = Vec::new();

        // Check buffer first
        if let Some(pos) = self.buffer.iter().position(|&b| b == byte) {
            result.extend(self.buffer.drain(..=pos));
            return Ok(result);
        }
        result.append(&mut self.buffer);

        // Keep reading until we find the byte
        let mut temp = [0u8; 256];
        loop {
            let n = self.read_bytes(&mut temp)?;
            if n == 0 {
                break;
            }
            if let Some(pos) = temp[..n].iter().position(|&b| b == byte) {
                result.extend_from_slice(&temp[..=pos]);
                self.buffer.extend_from_slice(&temp[pos + 1..n]);
                break;
            }
            result.extend_from_slice(&temp[..n]);
        }

        Ok(result)
    }

    /// Read a line (until newline).
    pub fn read_line(&mut self) -> Result<String> {
        let bytes = self.read_until(b'\n')?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

impl std::io::Read for RemoteSerialPort {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.read_bytes(buf)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
    }
}

impl std::io::Write for RemoteSerialPort {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.write_bytes(buf)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

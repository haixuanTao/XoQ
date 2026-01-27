//! Cross-platform CAN bus support.
//!
//! This module provides CAN bus access using the [`socketcan`](https://crates.io/crates/socketcan) crate,
//! which works on Linux systems with SocketCAN support.
//!
//! # Features
//!
//! - Blocking I/O on dedicated threads (doesn't block tokio runtime)
//! - Support for both standard CAN and CAN FD frames
//! - Split into separate read/write halves for concurrent access
//! - Interface enumeration to list available CAN interfaces
//!
//! # Example
//!
//! ```no_run
//! use xoq::can::{CanSocket, CanConfig, CanFrame};
//!
//! # async fn example() -> anyhow::Result<()> {
//! // Simple open with defaults
//! let socket = CanSocket::open_simple("can0")?;
//!
//! // Split for concurrent read/write
//! let (mut reader, mut writer) = socket.split();
//!
//! // Write a frame (async, but uses dedicated thread internally)
//! let frame = CanFrame::new(0x123, &[1, 2, 3, 4])?;
//! writer.write_frame(&frame).await?;
//!
//! // Read a frame
//! let frame = reader.read_frame().await?;
//! println!("Received: ID={:x} Data={:?}", frame.id(), frame.data());
//! # Ok(())
//! # }
//! ```
//!
//! # Listing Available Interfaces
//!
//! ```no_run
//! use xoq::can::list_interfaces;
//!
//! for iface in list_interfaces()? {
//!     println!("{}", iface.name);
//! }
//! # Ok::<(), anyhow::Error>(())
//! ```

use anyhow::Result;

/// Trait for CAN bus socket implementations.
///
/// This allows both local socketcan and remote xoq sockets to be used interchangeably.
/// Implementing this trait enables generic code to work with any CAN socket type.
///
/// # Example
///
/// ```no_run
/// use xoq::can::CanBusSocket;
///
/// fn send_command(socket: &impl CanBusSocket, can_id: u32, data: &[u8]) -> anyhow::Result<()> {
///     socket.write_raw(can_id, data)
/// }
/// ```
pub trait CanBusSocket: Send + Sync {
    /// Check if socket is open/connected.
    fn is_open(&self) -> bool;

    /// Write a raw CAN frame.
    fn write_raw(&self, can_id: u32, data: &[u8]) -> anyhow::Result<()>;

    /// Read a raw CAN frame. Returns None on timeout.
    fn read_raw(&self) -> anyhow::Result<Option<(u32, Vec<u8>)>>;

    /// Check if data is available with timeout (microseconds).
    fn is_data_available(&self, timeout_us: u64) -> anyhow::Result<bool>;

    /// Set receive timeout in microseconds.
    fn set_recv_timeout(&mut self, timeout_us: u64) -> anyhow::Result<()>;
}
use socketcan::frame::FdFlags;
use socketcan::{EmbeddedFrame, Frame, Socket};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// CAN socket configuration.
///
/// Specifies all parameters needed to open a CAN interface.
///
/// # Example
///
/// ```
/// use xoq::can::CanConfig;
///
/// // Simple config with defaults
/// let config = CanConfig::new("can0");
///
/// // Config with CAN FD enabled
/// let config = CanConfig {
///     interface: "can0".to_string(),
///     enable_fd: true,
///     read_timeout: std::time::Duration::from_millis(100),
/// };
/// ```
#[derive(Clone, Debug)]
pub struct CanConfig {
    /// Interface name (e.g., "can0", "vcan0")
    pub interface: String,
    /// Enable CAN FD support
    pub enable_fd: bool,
    /// Read timeout
    pub read_timeout: Duration,
}

impl CanConfig {
    /// Create a new config with defaults.
    pub fn new(interface: &str) -> Self {
        Self {
            interface: interface.to_string(),
            enable_fd: false,
            read_timeout: Duration::from_millis(100),
        }
    }

    /// Create a new config with CAN FD enabled.
    pub fn new_fd(interface: &str) -> Self {
        Self {
            interface: interface.to_string(),
            enable_fd: true,
            read_timeout: Duration::from_millis(100),
        }
    }
}

/// A standard CAN frame.
///
/// Supports up to 8 bytes of data.
#[derive(Clone, Debug)]
pub struct CanFrame {
    id: u32,
    data: Vec<u8>,
    is_extended: bool,
    is_remote: bool,
    is_error: bool,
}

impl CanFrame {
    /// Create a new standard CAN frame.
    ///
    /// # Arguments
    /// * `id` - CAN identifier (11-bit for standard, 29-bit for extended)
    /// * `data` - Frame data (up to 8 bytes)
    pub fn new(id: u32, data: &[u8]) -> Result<Self> {
        if data.len() > 8 {
            anyhow::bail!("CAN frame data cannot exceed 8 bytes");
        }
        Ok(Self {
            id,
            data: data.to_vec(),
            is_extended: id > 0x7FF,
            is_remote: false,
            is_error: false,
        })
    }

    /// Create a new extended CAN frame.
    pub fn new_extended(id: u32, data: &[u8]) -> Result<Self> {
        if data.len() > 8 {
            anyhow::bail!("CAN frame data cannot exceed 8 bytes");
        }
        Ok(Self {
            id,
            data: data.to_vec(),
            is_extended: true,
            is_remote: false,
            is_error: false,
        })
    }

    /// Create a remote transmission request frame.
    pub fn new_remote(id: u32, dlc: u8) -> Result<Self> {
        if dlc > 8 {
            anyhow::bail!("CAN RTR DLC cannot exceed 8");
        }
        Ok(Self {
            id,
            data: vec![0; dlc as usize],
            is_extended: id > 0x7FF,
            is_remote: true,
            is_error: false,
        })
    }

    /// Get the CAN identifier.
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Get the frame data.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Check if this is an extended frame (29-bit ID).
    pub fn is_extended(&self) -> bool {
        self.is_extended
    }

    /// Check if this is a remote transmission request.
    pub fn is_remote(&self) -> bool {
        self.is_remote
    }

    /// Check if this is an error frame.
    pub fn is_error(&self) -> bool {
        self.is_error
    }

    /// Get the data length code.
    pub fn dlc(&self) -> u8 {
        self.data.len() as u8
    }
}

impl TryFrom<socketcan::CanFrame> for CanFrame {
    type Error = anyhow::Error;

    fn try_from(frame: socketcan::CanFrame) -> Result<Self> {
        // CanFrame is an enum, extract the data frame
        match frame {
            socketcan::CanFrame::Data(data_frame) => Self::try_from(data_frame),
            socketcan::CanFrame::Remote(remote_frame) => Ok(Self {
                id: remote_frame.raw_id(),
                data: vec![0; remote_frame.dlc()],
                is_extended: remote_frame.is_extended(),
                is_remote: true,
                is_error: false,
            }),
            socketcan::CanFrame::Error(_) => {
                anyhow::bail!("Cannot convert error frame to CanFrame")
            }
        }
    }
}

impl TryFrom<socketcan::CanDataFrame> for CanFrame {
    type Error = anyhow::Error;

    fn try_from(frame: socketcan::CanDataFrame) -> Result<Self> {
        Ok(Self {
            id: frame.raw_id(),
            data: frame.data().to_vec(),
            is_extended: frame.is_extended(),
            is_remote: false,
            is_error: false,
        })
    }
}

impl TryFrom<&CanFrame> for socketcan::CanFrame {
    type Error = anyhow::Error;

    fn try_from(frame: &CanFrame) -> Result<Self> {
        if frame.is_extended {
            let id = socketcan::ExtendedId::new(frame.id)
                .ok_or_else(|| anyhow::anyhow!("Invalid extended CAN ID: {}", frame.id))?;
            let data_frame = socketcan::CanDataFrame::new(id, &frame.data)
                .ok_or_else(|| anyhow::anyhow!("Failed to create CAN frame"))?;
            Ok(socketcan::CanFrame::Data(data_frame))
        } else {
            let id = socketcan::StandardId::new(frame.id as u16)
                .ok_or_else(|| anyhow::anyhow!("Invalid standard CAN ID: {}", frame.id))?;
            let data_frame = socketcan::CanDataFrame::new(id, &frame.data)
                .ok_or_else(|| anyhow::anyhow!("Failed to create CAN frame"))?;
            Ok(socketcan::CanFrame::Data(data_frame))
        }
    }
}

/// A CAN FD frame.
///
/// Supports up to 64 bytes of data with flexible data rate.
#[derive(Clone, Debug)]
pub struct CanFdFrame {
    id: u32,
    data: Vec<u8>,
    is_extended: bool,
    flags: CanFdFlags,
}

/// CAN FD specific flags.
#[derive(Clone, Copy, Debug, Default)]
pub struct CanFdFlags {
    /// Bit rate switch - data phase at higher bit rate
    pub brs: bool,
    /// Error state indicator
    pub esi: bool,
}

impl CanFdFrame {
    /// Create a new CAN FD frame.
    ///
    /// # Arguments
    /// * `id` - CAN identifier
    /// * `data` - Frame data (up to 64 bytes)
    pub fn new(id: u32, data: &[u8]) -> Result<Self> {
        if data.len() > 64 {
            anyhow::bail!("CAN FD frame data cannot exceed 64 bytes");
        }
        Ok(Self {
            id,
            data: data.to_vec(),
            is_extended: id > 0x7FF,
            flags: CanFdFlags::default(),
        })
    }

    /// Create a new CAN FD frame with flags.
    pub fn new_with_flags(id: u32, data: &[u8], flags: CanFdFlags) -> Result<Self> {
        if data.len() > 64 {
            anyhow::bail!("CAN FD frame data cannot exceed 64 bytes");
        }
        Ok(Self {
            id,
            data: data.to_vec(),
            is_extended: id > 0x7FF,
            flags,
        })
    }

    /// Get the CAN identifier.
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Get the frame data.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Check if this is an extended frame.
    pub fn is_extended(&self) -> bool {
        self.is_extended
    }

    /// Get the FD flags.
    pub fn flags(&self) -> CanFdFlags {
        self.flags
    }

    /// Get the data length.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Check if the frame has no data.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

impl TryFrom<socketcan::CanFdFrame> for CanFdFrame {
    type Error = anyhow::Error;

    fn try_from(frame: socketcan::CanFdFrame) -> Result<Self> {
        Ok(Self {
            id: frame.raw_id(),
            data: frame.data().to_vec(),
            is_extended: frame.is_extended(),
            flags: CanFdFlags {
                brs: frame.is_brs(),
                esi: frame.is_esi(),
            },
        })
    }
}

impl TryFrom<&CanFdFrame> for socketcan::CanFdFrame {
    type Error = anyhow::Error;

    fn try_from(frame: &CanFdFrame) -> Result<Self> {
        let fd_flags = FdFlags::from_bits_truncate(
            if frame.flags.brs { FdFlags::BRS.bits() } else { 0 }
                | if frame.flags.esi { FdFlags::ESI.bits() } else { 0 },
        );
        if frame.is_extended {
            let id = socketcan::ExtendedId::new(frame.id)
                .ok_or_else(|| anyhow::anyhow!("Invalid extended CAN ID: {}", frame.id))?;
            socketcan::CanFdFrame::with_flags(id, &frame.data, fd_flags)
                .ok_or_else(|| anyhow::anyhow!("Failed to create CAN FD frame"))
        } else {
            let id = socketcan::StandardId::new(frame.id as u16)
                .ok_or_else(|| anyhow::anyhow!("Invalid standard CAN ID: {}", frame.id))?;
            socketcan::CanFdFrame::with_flags(id, &frame.data, fd_flags)
                .ok_or_else(|| anyhow::anyhow!("Failed to create CAN FD frame"))
        }
    }
}

/// Either a standard CAN frame or a CAN FD frame.
#[derive(Clone, Debug)]
pub enum AnyCanFrame {
    /// Standard CAN frame (up to 8 bytes)
    Can(CanFrame),
    /// CAN FD frame (up to 64 bytes)
    CanFd(CanFdFrame),
}

impl AnyCanFrame {
    /// Get the CAN identifier.
    pub fn id(&self) -> u32 {
        match self {
            AnyCanFrame::Can(f) => f.id(),
            AnyCanFrame::CanFd(f) => f.id(),
        }
    }

    /// Get the frame data.
    pub fn data(&self) -> &[u8] {
        match self {
            AnyCanFrame::Can(f) => f.data(),
            AnyCanFrame::CanFd(f) => f.data(),
        }
    }

    /// Check if this is a CAN FD frame.
    pub fn is_fd(&self) -> bool {
        matches!(self, AnyCanFrame::CanFd(_))
    }
}

/// Internal enum to hold either socket type
enum InnerSocket {
    Can(socketcan::CanSocket),
    CanFd(socketcan::CanFdSocket),
}

/// A CAN socket that can be split into read/write halves.
///
/// Uses blocking I/O on dedicated threads to avoid blocking the tokio runtime.
pub struct CanSocket {
    socket: InnerSocket,
    interface: String,
    fd_enabled: bool,
}

impl CanSocket {
    /// Open a CAN interface with the given configuration.
    pub fn open(config: &CanConfig) -> Result<Self> {
        let (socket, fd_enabled) = if config.enable_fd {
            let socket = socketcan::CanFdSocket::open(&config.interface)
                .map_err(|e| anyhow::anyhow!("Failed to open CAN FD interface {}: {}", config.interface, e))?;
            socket
                .set_read_timeout(config.read_timeout)
                .map_err(|e| anyhow::anyhow!("Failed to set read timeout: {}", e))?;
            (InnerSocket::CanFd(socket), true)
        } else {
            let socket = socketcan::CanSocket::open(&config.interface)
                .map_err(|e| anyhow::anyhow!("Failed to open CAN interface {}: {}", config.interface, e))?;
            socket
                .set_read_timeout(config.read_timeout)
                .map_err(|e| anyhow::anyhow!("Failed to set read timeout: {}", e))?;
            (InnerSocket::Can(socket), false)
        };

        Ok(Self {
            socket,
            interface: config.interface.clone(),
            fd_enabled,
        })
    }

    /// Open a CAN interface with default settings.
    pub fn open_simple(interface: &str) -> Result<Self> {
        let config = CanConfig::new(interface);
        Self::open(&config)
    }

    /// Open a CAN interface with CAN FD enabled.
    pub fn open_fd(interface: &str) -> Result<Self> {
        let config = CanConfig::new_fd(interface);
        Self::open(&config)
    }

    /// Check if CAN FD mode is enabled.
    pub fn is_fd_enabled(&self) -> bool {
        self.fd_enabled
    }

    /// Split into read and write halves.
    ///
    /// Each half runs blocking I/O on dedicated threads, communicating via channels.
    /// This allows concurrent reading and writing without blocking the tokio runtime.
    pub fn split(self) -> (CanReader, CanWriter) {
        // Create channels for async bridge
        let (read_tx, read_rx) = mpsc::channel::<ReadResult>();
        let (read_cmd_tx, read_cmd_rx) = mpsc::channel::<ReadCommand>();
        let (write_tx, write_rx) = mpsc::channel::<WriteCommand>();
        let (write_result_tx, write_result_rx) = mpsc::channel::<WriteResult>();

        let interface = self.interface.clone();
        let fd_enabled = self.fd_enabled;

        // Spawn reader thread
        match self.socket {
            InnerSocket::Can(reader_socket) => {
                thread::spawn(move || {
                    while let Ok(cmd) = read_cmd_rx.recv() {
                        match cmd {
                            ReadCommand::Read => {
                                match reader_socket.read_frame() {
                                    Ok(frame) => {
                                        match CanFrame::try_from(frame) {
                                            Ok(cf) => {
                                                if read_tx.send(ReadResult::Frame(AnyCanFrame::Can(cf))).is_err() {
                                                    break;
                                                }
                                            }
                                            Err(e) => {
                                                if read_tx.send(ReadResult::Error(e.to_string())).is_err() {
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
                                        || e.kind() == std::io::ErrorKind::TimedOut =>
                                    {
                                        if read_tx.send(ReadResult::Timeout).is_err() {
                                            break;
                                        }
                                    }
                                    Err(e) => {
                                        if read_tx.send(ReadResult::Error(e.to_string())).is_err() {
                                            break;
                                        }
                                    }
                                }
                            }
                            ReadCommand::Stop => break,
                        }
                    }
                });
            }
            InnerSocket::CanFd(reader_socket) => {
                thread::spawn(move || {
                    while let Ok(cmd) = read_cmd_rx.recv() {
                        match cmd {
                            ReadCommand::Read => {
                                match reader_socket.read_frame() {
                                    Ok(frame) => {
                                        let any_frame = match frame {
                                            socketcan::CanAnyFrame::Normal(f) => {
                                                match CanFrame::try_from(f) {
                                                    Ok(cf) => AnyCanFrame::Can(cf),
                                                    Err(e) => {
                                                        if read_tx.send(ReadResult::Error(e.to_string())).is_err() {
                                                            break;
                                                        }
                                                        continue;
                                                    }
                                                }
                                            }
                                            socketcan::CanAnyFrame::Fd(f) => {
                                                match CanFdFrame::try_from(f) {
                                                    Ok(cf) => AnyCanFrame::CanFd(cf),
                                                    Err(e) => {
                                                        if read_tx.send(ReadResult::Error(e.to_string())).is_err() {
                                                            break;
                                                        }
                                                        continue;
                                                    }
                                                }
                                            }
                                            socketcan::CanAnyFrame::Remote(_) => {
                                                continue; // Skip remote frames
                                            }
                                            socketcan::CanAnyFrame::Error(_) => {
                                                continue; // Skip error frames
                                            }
                                        };
                                        if read_tx.send(ReadResult::Frame(any_frame)).is_err() {
                                            break;
                                        }
                                    }
                                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
                                        || e.kind() == std::io::ErrorKind::TimedOut =>
                                    {
                                        if read_tx.send(ReadResult::Timeout).is_err() {
                                            break;
                                        }
                                    }
                                    Err(e) => {
                                        if read_tx.send(ReadResult::Error(e.to_string())).is_err() {
                                            break;
                                        }
                                    }
                                }
                            }
                            ReadCommand::Stop => break,
                        }
                    }
                });
            }
        }

        // Spawn writer thread - open a new socket for writing
        let interface_for_writer = interface;
        let fd_for_writer = fd_enabled;
        thread::spawn(move || {
            if fd_for_writer {
                let writer_socket = match socketcan::CanFdSocket::open(&interface_for_writer) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!("Failed to open CAN FD writer socket: {}", e);
                        return;
                    }
                };
                while let Ok(cmd) = write_rx.recv() {
                    match cmd {
                        WriteCommand::WriteFrame(frame) => {
                            let result = match &frame {
                                AnyCanFrame::Can(f) => {
                                    match socketcan::CanFrame::try_from(f) {
                                        Ok(sf) => writer_socket.write_frame(&sf),
                                        Err(e) => {
                                            let _ = write_result_tx.send(WriteResult::Error(e.to_string()));
                                            continue;
                                        }
                                    }
                                }
                                AnyCanFrame::CanFd(f) => {
                                    match socketcan::CanFdFrame::try_from(f) {
                                        Ok(sf) => writer_socket.write_frame(&sf),
                                        Err(e) => {
                                            let _ = write_result_tx.send(WriteResult::Error(e.to_string()));
                                            continue;
                                        }
                                    }
                                }
                            };
                            let _ = write_result_tx.send(match result {
                                Ok(()) => WriteResult::Ok,
                                Err(e) => WriteResult::Error(e.to_string()),
                            });
                        }
                        WriteCommand::Stop => break,
                    }
                }
            } else {
                let writer_socket = match socketcan::CanSocket::open(&interface_for_writer) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!("Failed to open CAN writer socket: {}", e);
                        return;
                    }
                };
                while let Ok(cmd) = write_rx.recv() {
                    match cmd {
                        WriteCommand::WriteFrame(frame) => {
                            let result = match &frame {
                                AnyCanFrame::Can(f) => {
                                    match socketcan::CanFrame::try_from(f) {
                                        Ok(sf) => writer_socket.write_frame(&sf),
                                        Err(e) => {
                                            let _ = write_result_tx.send(WriteResult::Error(e.to_string()));
                                            continue;
                                        }
                                    }
                                }
                                AnyCanFrame::CanFd(_) => {
                                    let _ = write_result_tx.send(WriteResult::Error(
                                        "CAN FD frames not supported on standard CAN socket".to_string()
                                    ));
                                    continue;
                                }
                            };
                            let _ = write_result_tx.send(match result {
                                Ok(()) => WriteResult::Ok,
                                Err(e) => WriteResult::Error(e.to_string()),
                            });
                        }
                        WriteCommand::Stop => break,
                    }
                }
            }
        });

        (
            CanReader {
                read_rx,
                read_cmd_tx,
            },
            CanWriter {
                write_tx,
                write_result_rx,
            },
        )
    }
}

// Internal message types for thread communication
enum ReadCommand {
    Read,
    Stop,
}

enum ReadResult {
    Frame(AnyCanFrame),
    Timeout,
    Error(String),
}

enum WriteCommand {
    WriteFrame(AnyCanFrame),
    Stop,
}

enum WriteResult {
    Ok,
    Error(String),
}

/// Read half of a split CAN socket.
///
/// Uses a dedicated thread for blocking reads, bridged to async via channels.
pub struct CanReader {
    read_rx: mpsc::Receiver<ReadResult>,
    read_cmd_tx: mpsc::Sender<ReadCommand>,
}

impl CanReader {
    /// Read a CAN frame from the socket.
    ///
    /// Returns `None` if no frame is available (timeout).
    pub async fn read_frame(&mut self) -> Result<Option<AnyCanFrame>> {
        // Send read command to dedicated thread
        self.read_cmd_tx
            .send(ReadCommand::Read)
            .map_err(|_| anyhow::anyhow!("CAN reader thread died"))?;

        // Wait for result (using spawn_blocking to not block tokio)
        let rx = unsafe {
            // SAFETY: We're moving the receiver to spawn_blocking and back.
            // This is safe because we wait for the result before using rx again.
            std::ptr::read(&self.read_rx)
        };

        let (result, rx) = tokio::task::spawn_blocking(move || {
            let result = rx.recv();
            (result, rx)
        })
        .await?;

        // Restore receiver
        unsafe {
            std::ptr::write(&mut self.read_rx, rx);
        }

        match result {
            Ok(ReadResult::Frame(frame)) => Ok(Some(frame)),
            Ok(ReadResult::Timeout) => Ok(None),
            Ok(ReadResult::Error(e)) => Err(anyhow::anyhow!("CAN read error: {}", e)),
            Err(_) => Err(anyhow::anyhow!("CAN reader thread died")),
        }
    }
}

impl Drop for CanReader {
    fn drop(&mut self) {
        let _ = self.read_cmd_tx.send(ReadCommand::Stop);
    }
}

/// Write half of a split CAN socket.
///
/// Uses a dedicated thread for blocking writes, bridged to async via channels.
pub struct CanWriter {
    write_tx: mpsc::Sender<WriteCommand>,
    write_result_rx: mpsc::Receiver<WriteResult>,
}

impl CanWriter {
    /// Write a standard CAN frame to the socket.
    pub async fn write_frame(&mut self, frame: &CanFrame) -> Result<()> {
        self.write_any_frame(AnyCanFrame::Can(frame.clone())).await
    }

    /// Write a CAN FD frame to the socket.
    pub async fn write_fd_frame(&mut self, frame: &CanFdFrame) -> Result<()> {
        self.write_any_frame(AnyCanFrame::CanFd(frame.clone())).await
    }

    /// Write any CAN frame to the socket.
    pub async fn write_any_frame(&mut self, frame: AnyCanFrame) -> Result<()> {
        // Send write command to dedicated thread
        self.write_tx
            .send(WriteCommand::WriteFrame(frame))
            .map_err(|_| anyhow::anyhow!("CAN writer thread died"))?;

        // Wait for result
        let rx = unsafe { std::ptr::read(&self.write_result_rx) };

        let (result, rx) = tokio::task::spawn_blocking(move || {
            let result = rx.recv();
            (result, rx)
        })
        .await?;

        unsafe {
            std::ptr::write(&mut self.write_result_rx, rx);
        }

        match result {
            Ok(WriteResult::Ok) => Ok(()),
            Ok(WriteResult::Error(e)) => Err(anyhow::anyhow!("CAN write error: {}", e)),
            Err(_) => Err(anyhow::anyhow!("CAN writer thread died")),
        }
    }
}

impl Drop for CanWriter {
    fn drop(&mut self) {
        let _ = self.write_tx.send(WriteCommand::Stop);
    }
}

/// Information about a CAN interface.
#[derive(Clone, Debug)]
pub struct CanInterfaceInfo {
    /// Interface name (e.g., "can0", "vcan0")
    pub name: String,
}

/// List available CAN interfaces on the system.
///
/// This function reads from /sys/class/net to find CAN interfaces.
///
/// # Example
///
/// ```no_run
/// use xoq::can::list_interfaces;
///
/// for iface in list_interfaces()? {
///     println!("CAN interface: {}", iface.name);
/// }
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn list_interfaces() -> Result<Vec<CanInterfaceInfo>> {
    let mut interfaces = Vec::new();

    // Read /sys/class/net directory
    let net_dir = std::fs::read_dir("/sys/class/net")?;

    for entry in net_dir {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();

        // Check if this is a CAN interface by looking at the type
        let type_path = entry.path().join("type");
        if let Ok(type_str) = std::fs::read_to_string(&type_path) {
            let type_num: u32 = type_str.trim().parse().unwrap_or(0);
            // ARPHRD_CAN = 280
            if type_num == 280 {
                interfaces.push(CanInterfaceInfo { name });
            }
        }
    }

    Ok(interfaces)
}

/// Network frame encoding for CAN frames.
///
/// Frame format:
/// ```text
/// [1 byte flags][4 bytes can_id (little-endian)][1 byte data_len][0-64 bytes data]
/// ```
///
/// Flags:
/// - bit 0: CAN FD frame
/// - bit 1: Extended ID
/// - bits 2-7: reserved
pub mod wire {
    use super::{AnyCanFrame, CanFdFlags, CanFdFrame, CanFrame};
    use anyhow::Result;

    const FLAG_FD: u8 = 0x01;
    const FLAG_EXTENDED: u8 = 0x02;
    const FLAG_BRS: u8 = 0x04;
    const FLAG_ESI: u8 = 0x08;

    /// Encode a CAN frame for network transmission.
    pub fn encode(frame: &AnyCanFrame) -> Vec<u8> {
        let mut buf = Vec::with_capacity(70);

        let (flags, id, data) = match frame {
            AnyCanFrame::Can(f) => {
                let mut flags = 0u8;
                if f.is_extended() {
                    flags |= FLAG_EXTENDED;
                }
                (flags, f.id(), f.data())
            }
            AnyCanFrame::CanFd(f) => {
                let mut flags = FLAG_FD;
                if f.is_extended() {
                    flags |= FLAG_EXTENDED;
                }
                if f.flags().brs {
                    flags |= FLAG_BRS;
                }
                if f.flags().esi {
                    flags |= FLAG_ESI;
                }
                (flags, f.id(), f.data())
            }
        };

        buf.push(flags);
        buf.extend_from_slice(&id.to_le_bytes());
        buf.push(data.len() as u8);
        buf.extend_from_slice(data);

        buf
    }

    /// Decode a CAN frame from network data.
    ///
    /// Returns the decoded frame and the number of bytes consumed.
    pub fn decode(data: &[u8]) -> Result<(AnyCanFrame, usize)> {
        if data.len() < 6 {
            anyhow::bail!("Insufficient data for CAN frame header");
        }

        let flags = data[0];
        let id = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
        let data_len = data[5] as usize;

        if data.len() < 6 + data_len {
            anyhow::bail!("Insufficient data for CAN frame payload");
        }

        let frame_data = &data[6..6 + data_len];
        let is_fd = (flags & FLAG_FD) != 0;
        let is_extended = (flags & FLAG_EXTENDED) != 0;

        let frame = if is_fd {
            let fd_flags = CanFdFlags {
                brs: (flags & FLAG_BRS) != 0,
                esi: (flags & FLAG_ESI) != 0,
            };
            let mut f = CanFdFrame::new_with_flags(id, frame_data, fd_flags)?;
            if is_extended {
                f.is_extended = true;
            }
            AnyCanFrame::CanFd(f)
        } else {
            let mut f = CanFrame::new(id, frame_data)?;
            f.is_extended = is_extended;
            AnyCanFrame::Can(f)
        };

        Ok((frame, 6 + data_len))
    }

    /// Get the expected size of an encoded frame given its header.
    pub fn encoded_size(header: &[u8]) -> Result<usize> {
        if header.len() < 6 {
            anyhow::bail!("Insufficient data for CAN frame header");
        }
        let data_len = header[5] as usize;
        Ok(6 + data_len)
    }
}

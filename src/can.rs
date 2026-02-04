//! Local CAN bus support using Linux SocketCAN.
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
use socketcan::frame::FdFlags;
use socketcan::{EmbeddedFrame, Frame, Socket};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

// Re-export types from can_types for backwards compatibility
pub use crate::can_types::{
    wire, AnyCanFrame, CanBusSocket, CanFdFlags, CanFdFrame, CanFrame, CanInterfaceInfo,
};

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

// Conversion implementations for socketcan types
impl TryFrom<socketcan::CanFrame> for CanFrame {
    type Error = anyhow::Error;

    fn try_from(frame: socketcan::CanFrame) -> Result<Self> {
        match frame {
            socketcan::CanFrame::Data(data_frame) => Self::try_from(data_frame),
            socketcan::CanFrame::Remote(remote_frame) => Ok(Self::new_remote(
                remote_frame.raw_id(),
                remote_frame.dlc() as u8,
            )?),
            socketcan::CanFrame::Error(_) => {
                anyhow::bail!("Cannot convert error frame to CanFrame")
            }
        }
    }
}

impl TryFrom<socketcan::CanDataFrame> for CanFrame {
    type Error = anyhow::Error;

    fn try_from(frame: socketcan::CanDataFrame) -> Result<Self> {
        let mut f = Self::new(frame.raw_id(), frame.data())?;
        f.is_extended = frame.is_extended();
        Ok(f)
    }
}

impl TryFrom<&CanFrame> for socketcan::CanFrame {
    type Error = anyhow::Error;

    fn try_from(frame: &CanFrame) -> Result<Self> {
        if frame.is_extended() {
            let id = socketcan::ExtendedId::new(frame.id())
                .ok_or_else(|| anyhow::anyhow!("Invalid extended CAN ID: {}", frame.id()))?;
            let data_frame = socketcan::CanDataFrame::new(id, frame.data())
                .ok_or_else(|| anyhow::anyhow!("Failed to create CAN frame"))?;
            Ok(socketcan::CanFrame::Data(data_frame))
        } else {
            let id = socketcan::StandardId::new(frame.id() as u16)
                .ok_or_else(|| anyhow::anyhow!("Invalid standard CAN ID: {}", frame.id()))?;
            let data_frame = socketcan::CanDataFrame::new(id, frame.data())
                .ok_or_else(|| anyhow::anyhow!("Failed to create CAN frame"))?;
            Ok(socketcan::CanFrame::Data(data_frame))
        }
    }
}

impl TryFrom<socketcan::CanFdFrame> for CanFdFrame {
    type Error = anyhow::Error;

    fn try_from(frame: socketcan::CanFdFrame) -> Result<Self> {
        Ok(Self::new_with_flags(
            frame.raw_id(),
            frame.data(),
            CanFdFlags {
                brs: frame.is_brs(),
                esi: frame.is_esi(),
            },
        )?)
    }
}

impl TryFrom<&CanFdFrame> for socketcan::CanFdFrame {
    type Error = anyhow::Error;

    fn try_from(frame: &CanFdFrame) -> Result<Self> {
        let fd_flags = FdFlags::from_bits_truncate(
            if frame.flags().brs {
                FdFlags::BRS.bits()
            } else {
                0
            } | if frame.flags().esi {
                FdFlags::ESI.bits()
            } else {
                0
            },
        );
        if frame.is_extended() {
            let id = socketcan::ExtendedId::new(frame.id())
                .ok_or_else(|| anyhow::anyhow!("Invalid extended CAN ID: {}", frame.id()))?;
            socketcan::CanFdFrame::with_flags(id, frame.data(), fd_flags)
                .ok_or_else(|| anyhow::anyhow!("Failed to create CAN FD frame"))
        } else {
            let id = socketcan::StandardId::new(frame.id() as u16)
                .ok_or_else(|| anyhow::anyhow!("Invalid standard CAN ID: {}", frame.id()))?;
            socketcan::CanFdFrame::with_flags(id, frame.data(), fd_flags)
                .ok_or_else(|| anyhow::anyhow!("Failed to create CAN FD frame"))
        }
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
            let socket = socketcan::CanFdSocket::open(&config.interface).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to open CAN FD interface {}: {}",
                    config.interface,
                    e
                )
            })?;
            socket
                .set_read_timeout(config.read_timeout)
                .map_err(|e| anyhow::anyhow!("Failed to set read timeout: {}", e))?;
            (InnerSocket::CanFd(socket), true)
        } else {
            let socket = socketcan::CanSocket::open(&config.interface).map_err(|e| {
                anyhow::anyhow!("Failed to open CAN interface {}: {}", config.interface, e)
            })?;
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
                            ReadCommand::Read => match reader_socket.read_frame() {
                                Ok(frame) => match CanFrame::try_from(frame) {
                                    Ok(cf) => {
                                        if read_tx
                                            .send(ReadResult::Frame(AnyCanFrame::Can(cf)))
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    Err(e) => {
                                        if read_tx.send(ReadResult::Error(e.to_string())).is_err() {
                                            break;
                                        }
                                    }
                                },
                                Err(e)
                                    if e.kind() == std::io::ErrorKind::WouldBlock
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
                            },
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
                                                        if read_tx
                                                            .send(ReadResult::Error(e.to_string()))
                                                            .is_err()
                                                        {
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
                                                        if read_tx
                                                            .send(ReadResult::Error(e.to_string()))
                                                            .is_err()
                                                        {
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
                                    Err(e)
                                        if e.kind() == std::io::ErrorKind::WouldBlock
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
                                AnyCanFrame::Can(f) => match socketcan::CanFrame::try_from(f) {
                                    Ok(sf) => writer_socket.write_frame(&sf),
                                    Err(e) => {
                                        let _ =
                                            write_result_tx.send(WriteResult::Error(e.to_string()));
                                        continue;
                                    }
                                },
                                AnyCanFrame::CanFd(f) => match socketcan::CanFdFrame::try_from(f) {
                                    Ok(sf) => writer_socket.write_frame(&sf),
                                    Err(e) => {
                                        let _ =
                                            write_result_tx.send(WriteResult::Error(e.to_string()));
                                        continue;
                                    }
                                },
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
                                AnyCanFrame::Can(f) => match socketcan::CanFrame::try_from(f) {
                                    Ok(sf) => writer_socket.write_frame(&sf),
                                    Err(e) => {
                                        let _ =
                                            write_result_tx.send(WriteResult::Error(e.to_string()));
                                        continue;
                                    }
                                },
                                AnyCanFrame::CanFd(_) => {
                                    let _ = write_result_tx.send(WriteResult::Error(
                                        "CAN FD frames not supported on standard CAN socket"
                                            .to_string(),
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
        self.write_any_frame(AnyCanFrame::CanFd(frame.clone()))
            .await
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

//! Cross-platform serial port support.
//!
//! This module provides async serial port access using [`tokio-serial`](https://crates.io/crates/tokio-serial),
//! which works on Linux, macOS, and Windows.
//!
//! # Features
//!
//! - Async read/write operations via tokio
//! - Configurable baud rate, data bits, parity, and stop bits
//! - Split into separate read/write halves for concurrent access
//! - Port enumeration to list available serial ports
//!
//! # Example
//!
//! ```no_run
//! use wser::serial::{SerialPort, SerialConfig, baud};
//!
//! # async fn example() -> anyhow::Result<()> {
//! // Simple open with defaults (8N1)
//! let mut port = SerialPort::open_simple("/dev/ttyUSB0", baud::B115200)?;
//!
//! // Write data
//! port.write_str("AT\r\n").await?;
//!
//! // Read response
//! let mut buf = [0u8; 256];
//! let n = port.read(&mut buf).await?;
//! println!("Received: {:?}", &buf[..n]);
//!
//! // Or use full configuration
//! let config = SerialConfig::new("/dev/ttyUSB0", 115200);
//! let port = SerialPort::open(&config)?;
//! # Ok(())
//! # }
//! ```
//!
//! # Splitting for Concurrent Access
//!
//! ```no_run
//! use wser::serial::SerialPort;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let port = SerialPort::open_simple("/dev/ttyUSB0", 115200)?;
//! let (mut reader, mut writer) = port.split();
//!
//! // Now reader and writer can be used from different tasks
//! tokio::spawn(async move {
//!     let mut buf = [0u8; 256];
//!     loop {
//!         let n = reader.read(&mut buf).await.unwrap();
//!         println!("Received: {:?}", &buf[..n]);
//!     }
//! });
//!
//! writer.write_str("Hello\r\n").await?;
//! # Ok(())
//! # }
//! ```
//!
//! # Listing Available Ports
//!
//! ```no_run
//! use wser::serial::list_ports;
//!
//! for port in list_ports()? {
//!     println!("{} - {:?}", port.name, port.port_type);
//! }
//! # Ok::<(), anyhow::Error>(())
//! ```

use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_serial::{SerialPortBuilderExt, SerialStream};

/// Common baud rates as constants.
///
/// # Example
///
/// ```
/// use wser::serial::baud;
///
/// let rate = baud::B115200; // 115200 bps
/// ```
pub mod baud {
    /// 9600 baud
    pub const B9600: u32 = 9600;
    /// 19200 baud
    pub const B19200: u32 = 19200;
    /// 38400 baud
    pub const B38400: u32 = 38400;
    /// 57600 baud
    pub const B57600: u32 = 57600;
    /// 115200 baud (most common)
    pub const B115200: u32 = 115200;
    /// 230400 baud
    pub const B230400: u32 = 230400;
    /// 460800 baud
    pub const B460800: u32 = 460800;
    /// 921600 baud
    pub const B921600: u32 = 921600;
}

/// Serial port configuration.
///
/// Specifies all parameters needed to open a serial port connection.
/// Use [`SerialConfig::new`] for common defaults (8 data bits, no parity, 1 stop bit).
///
/// # Example
///
/// ```
/// use wser::serial::{SerialConfig, DataBits, Parity, StopBits};
///
/// // Simple config with defaults (8N1)
/// let config = SerialConfig::new("/dev/ttyUSB0", 115200);
///
/// // Full custom config
/// let config = SerialConfig {
///     port: "/dev/ttyUSB0".to_string(),
///     baud_rate: 9600,
///     data_bits: DataBits::Seven,
///     parity: Parity::Even,
///     stop_bits: StopBits::One,
/// };
/// ```
#[derive(Clone, Debug)]
pub struct SerialConfig {
    /// Port name (e.g., "/dev/ttyUSB0" on Linux, "COM3" on Windows)
    pub port: String,
    /// Baud rate (e.g., 9600, 115200)
    pub baud_rate: u32,
    /// Number of data bits per character
    pub data_bits: DataBits,
    /// Parity checking mode
    pub parity: Parity,
    /// Number of stop bits
    pub stop_bits: StopBits,
}

impl SerialConfig {
    /// Create a new config with common defaults (8N1).
    ///
    /// Uses 8 data bits, no parity, and 1 stop bit - the most common configuration.
    pub fn new(port: &str, baud_rate: u32) -> Self {
        Self {
            port: port.to_string(),
            baud_rate,
            data_bits: DataBits::Eight,
            parity: Parity::None,
            stop_bits: StopBits::One,
        }
    }
}

/// Number of data bits per character.
///
/// Most devices use 8 data bits (the default).
#[derive(Clone, Copy, Debug, Default)]
pub enum DataBits {
    /// 5 data bits
    Five,
    /// 6 data bits
    Six,
    /// 7 data bits (common for ASCII text)
    Seven,
    /// 8 data bits (most common, default)
    #[default]
    Eight,
}

impl From<DataBits> for tokio_serial::DataBits {
    fn from(db: DataBits) -> Self {
        match db {
            DataBits::Five => tokio_serial::DataBits::Five,
            DataBits::Six => tokio_serial::DataBits::Six,
            DataBits::Seven => tokio_serial::DataBits::Seven,
            DataBits::Eight => tokio_serial::DataBits::Eight,
        }
    }
}

/// Parity checking mode.
///
/// Parity is an error-detection mechanism. Most modern devices use no parity (the default).
#[derive(Clone, Copy, Debug, Default)]
pub enum Parity {
    /// No parity bit (most common, default)
    #[default]
    None,
    /// Odd parity - parity bit set so total 1-bits is odd
    Odd,
    /// Even parity - parity bit set so total 1-bits is even
    Even,
}

impl From<Parity> for tokio_serial::Parity {
    fn from(p: Parity) -> Self {
        match p {
            Parity::None => tokio_serial::Parity::None,
            Parity::Odd => tokio_serial::Parity::Odd,
            Parity::Even => tokio_serial::Parity::Even,
        }
    }
}

/// Number of stop bits.
///
/// Stop bits signal the end of a character. Most devices use 1 stop bit (the default).
#[derive(Clone, Copy, Debug, Default)]
pub enum StopBits {
    /// 1 stop bit (most common, default)
    #[default]
    One,
    /// 2 stop bits
    Two,
}

impl From<StopBits> for tokio_serial::StopBits {
    fn from(sb: StopBits) -> Self {
        match sb {
            StopBits::One => tokio_serial::StopBits::One,
            StopBits::Two => tokio_serial::StopBits::Two,
        }
    }
}

/// An async serial port connection.
///
/// Provides async read/write access to a serial port. Can be split into
/// separate [`SerialReader`] and [`SerialWriter`] halves for concurrent access.
///
/// # Example
///
/// ```no_run
/// use wser::serial::SerialPort;
///
/// # async fn example() -> anyhow::Result<()> {
/// let mut port = SerialPort::open_simple("/dev/ttyUSB0", 115200)?;
///
/// // Write a command
/// port.write_str("AT\r\n").await?;
///
/// // Read response
/// let mut buf = [0u8; 256];
/// let n = port.read(&mut buf).await?;
/// println!("Response: {}", String::from_utf8_lossy(&buf[..n]));
/// # Ok(())
/// # }
/// ```
pub struct SerialPort {
    inner: SerialStream,
}

impl SerialPort {
    /// Open a serial port with the given configuration.
    pub fn open(config: &SerialConfig) -> Result<Self> {
        let port = tokio_serial::new(&config.port, config.baud_rate)
            .data_bits(config.data_bits.into())
            .parity(config.parity.into())
            .stop_bits(config.stop_bits.into())
            .open_native_async()?;

        Ok(Self { inner: port })
    }

    /// Open a serial port with default settings (8N1)
    pub fn open_simple(port: &str, baud_rate: u32) -> Result<Self> {
        let config = SerialConfig::new(port, baud_rate);
        Self::open(&config)
    }

    /// Write data to the serial port
    pub async fn write(&mut self, data: &[u8]) -> Result<usize> {
        let n = self.inner.write(data).await?;
        Ok(n)
    }

    /// Write all data to the serial port
    pub async fn write_all(&mut self, data: &[u8]) -> Result<()> {
        self.inner.write_all(data).await?;
        Ok(())
    }

    /// Write a string to the serial port
    pub async fn write_str(&mut self, data: &str) -> Result<()> {
        self.write_all(data.as_bytes()).await
    }

    /// Read data from the serial port
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let n = self.inner.read(buf).await?;
        Ok(n)
    }

    /// Read until buffer is full or EOF
    pub async fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        self.inner.read_exact(buf).await?;
        Ok(())
    }

    /// Split into read and write halves
    pub fn split(self) -> (SerialReader, SerialWriter) {
        let (reader, writer) = tokio::io::split(self.inner);
        (
            SerialReader { inner: reader },
            SerialWriter { inner: writer },
        )
    }
}

/// Read half of a split serial port.
///
/// Obtained by calling [`SerialPort::split`]. Can be used concurrently
/// with [`SerialWriter`] from different tasks.
pub struct SerialReader {
    inner: tokio::io::ReadHalf<SerialStream>,
}

impl SerialReader {
    /// Read data from the serial port.
    ///
    /// Returns the number of bytes read.
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let n = self.inner.read(buf).await?;
        Ok(n)
    }
}

/// Write half of a split serial port.
///
/// Obtained by calling [`SerialPort::split`]. Can be used concurrently
/// with [`SerialReader`] from different tasks.
pub struct SerialWriter {
    inner: tokio::io::WriteHalf<SerialStream>,
}

impl SerialWriter {
    /// Write data to the serial port.
    ///
    /// Returns the number of bytes written.
    pub async fn write(&mut self, data: &[u8]) -> Result<usize> {
        let n = self.inner.write(data).await?;
        Ok(n)
    }

    /// Write all data to the serial port.
    ///
    /// Continues writing until all bytes are sent.
    pub async fn write_all(&mut self, data: &[u8]) -> Result<()> {
        self.inner.write_all(data).await?;
        Ok(())
    }

    /// Write a UTF-8 string to the serial port.
    pub async fn write_str(&mut self, data: &str) -> Result<()> {
        self.write_all(data.as_bytes()).await
    }
}

/// List available serial ports on the system.
///
/// Returns information about each detected serial port including its name
/// and type (USB, PCI, Bluetooth, etc.).
///
/// # Example
///
/// ```no_run
/// use wser::serial::list_ports;
///
/// for port in list_ports()? {
///     println!("Port: {}", port.name);
///     match &port.port_type {
///         wser::serial::PortType::Usb { vid, pid, product, .. } => {
///             println!("  USB device: VID={:04x} PID={:04x}", vid, pid);
///             if let Some(name) = product {
///                 println!("  Product: {}", name);
///             }
///         }
///         _ => println!("  Type: {:?}", port.port_type),
///     }
/// }
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn list_ports() -> Result<Vec<SerialPortInfo>> {
    let ports = tokio_serial::available_ports()?;
    Ok(ports
        .into_iter()
        .map(|p| SerialPortInfo {
            name: p.port_name,
            port_type: match p.port_type {
                tokio_serial::SerialPortType::UsbPort(info) => PortType::Usb {
                    vid: info.vid,
                    pid: info.pid,
                    manufacturer: info.manufacturer,
                    product: info.product,
                },
                tokio_serial::SerialPortType::PciPort => PortType::Pci,
                tokio_serial::SerialPortType::BluetoothPort => PortType::Bluetooth,
                tokio_serial::SerialPortType::Unknown => PortType::Unknown,
            },
        })
        .collect())
}

/// Information about a detected serial port.
///
/// Returned by [`list_ports`].
#[derive(Clone, Debug)]
pub struct SerialPortInfo {
    /// Port name (e.g., "/dev/ttyUSB0" on Linux, "COM3" on Windows)
    pub name: String,
    /// Type of port (USB, PCI, Bluetooth, etc.)
    pub port_type: PortType,
}

/// Type of serial port hardware.
#[derive(Clone, Debug)]
pub enum PortType {
    /// USB serial adapter (most common for external devices)
    Usb {
        /// USB Vendor ID
        vid: u16,
        /// USB Product ID
        pid: u16,
        /// Manufacturer name (if available)
        manufacturer: Option<String>,
        /// Product name (if available)
        product: Option<String>,
    },
    /// PCI/PCIe serial card
    Pci,
    /// Bluetooth serial port
    Bluetooth,
    /// Unknown or unidentified port type
    Unknown,
}

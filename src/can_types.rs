//! Platform-independent CAN bus types and traits.
//!
//! This module contains CAN frame types and the `CanBusSocket` trait that work
//! on any platform. The actual socket implementations are platform-specific.

use anyhow::Result;

/// Trait for CAN bus socket implementations.
///
/// This allows both local socketcan and remote xoq sockets to be used interchangeably.
/// Implementing this trait enables generic code to work with any CAN socket type.
///
/// # Example
///
/// ```no_run
/// use xoq::can_types::CanBusSocket;
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

/// A standard CAN frame.
///
/// Supports up to 8 bytes of data.
#[derive(Clone, Debug)]
pub struct CanFrame {
    id: u32,
    data: Vec<u8>,
    pub(crate) is_extended: bool,
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

/// A CAN FD frame.
///
/// Supports up to 64 bytes of data with flexible data rate.
#[derive(Clone, Debug)]
pub struct CanFdFrame {
    id: u32,
    data: Vec<u8>,
    pub(crate) is_extended: bool,
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

/// Information about a CAN interface.
#[derive(Clone, Debug)]
pub struct CanInterfaceInfo {
    /// Interface name (e.g., "can0", "vcan0")
    pub name: String,
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

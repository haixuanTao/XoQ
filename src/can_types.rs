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

/// Network frame encoding using standard Linux `struct canfd_frame` layout.
///
/// ```text
/// struct canfd_frame {        // 72 bytes total, fixed size
///     u32 can_id;             // CAN ID + flags (LE). Bit 31=EFF, 30=RTR, 29=ERR
///     u8  len;                // payload length (0-64)
///     u8  flags;              // CANFD_BRS=0x01, CANFD_ESI=0x02
///     u8  __res0;             // reserved
///     u8  __res1;             // reserved
///     u8  data[64];           // payload, zero-padded
/// };
/// ```
pub mod wire {
    use super::{AnyCanFrame, CanFdFlags, CanFdFrame, CanFrame};
    use anyhow::Result;

    /// Fixed size of a canfd_frame (matches Linux struct canfd_frame).
    pub const FRAME_SIZE: usize = 72;

    // can_id flag bits (high bits of the 32-bit can_id field)
    const CAN_EFF_FLAG: u32 = 0x80000000; // Extended frame format
    const CAN_RTR_FLAG: u32 = 0x40000000; // Remote transmission request
    const CAN_EFF_MASK: u32 = 0x1FFFFFFF; // 29-bit extended ID mask
    const CAN_SFF_MASK: u32 = 0x000007FF; // 11-bit standard ID mask

    // canfd_frame flags byte
    const CANFD_BRS: u8 = 0x01; // Bit rate switch
    const CANFD_ESI: u8 = 0x02; // Error state indicator

    /// Encode a CAN frame as a 72-byte canfd_frame.
    pub fn encode(frame: &AnyCanFrame) -> Vec<u8> {
        let mut buf = vec![0u8; FRAME_SIZE];

        let (raw_id, flags, data) = match frame {
            AnyCanFrame::Can(f) => {
                let mut id = f.id();
                if f.is_extended() {
                    id |= CAN_EFF_FLAG;
                }
                if f.is_remote() {
                    id |= CAN_RTR_FLAG;
                }
                (id, 0u8, f.data())
            }
            AnyCanFrame::CanFd(f) => {
                let mut id = f.id();
                if f.is_extended() {
                    id |= CAN_EFF_FLAG;
                }
                let mut flags = 0u8;
                if f.flags().brs {
                    flags |= CANFD_BRS;
                }
                if f.flags().esi {
                    flags |= CANFD_ESI;
                }
                (id, flags, f.data())
            }
        };

        buf[0..4].copy_from_slice(&raw_id.to_le_bytes());
        buf[4] = data.len() as u8;
        buf[5] = flags;
        // buf[6], buf[7] = reserved (already 0)
        buf[8..8 + data.len()].copy_from_slice(data);

        buf
    }

    /// Decode a canfd_frame from network data.
    ///
    /// Returns the decoded frame and the number of bytes consumed (always FRAME_SIZE).
    pub fn decode(data: &[u8]) -> Result<(AnyCanFrame, usize)> {
        if data.len() < FRAME_SIZE {
            anyhow::bail!(
                "Need {} bytes for canfd_frame, got {}",
                FRAME_SIZE,
                data.len()
            );
        }

        let raw_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let len = (data[4] as usize).min(64);
        let flags = data[5];

        let is_extended = (raw_id & CAN_EFF_FLAG) != 0;
        let is_rtr = (raw_id & CAN_RTR_FLAG) != 0;
        let can_id = if is_extended {
            raw_id & CAN_EFF_MASK
        } else {
            raw_id & CAN_SFF_MASK
        };

        let frame_data = &data[8..8 + len];

        let frame = if flags != 0 || len > 8 {
            // CAN FD frame (has FD flags set, or data > 8 bytes)
            let fd_flags = CanFdFlags {
                brs: (flags & CANFD_BRS) != 0,
                esi: (flags & CANFD_ESI) != 0,
            };
            let mut f = CanFdFrame::new_with_flags(can_id, frame_data, fd_flags)?;
            f.is_extended = is_extended;
            AnyCanFrame::CanFd(f)
        } else {
            // Standard CAN frame
            let mut f = if is_rtr {
                CanFrame::new_remote(can_id, len as u8)?
            } else {
                CanFrame::new(can_id, frame_data)?
            };
            f.is_extended = is_extended;
            AnyCanFrame::Can(f)
        };

        Ok((frame, FRAME_SIZE))
    }

    /// Get the expected size of an encoded frame (always FRAME_SIZE).
    pub fn encoded_size(_header: &[u8]) -> Result<usize> {
        Ok(FRAME_SIZE)
    }
}

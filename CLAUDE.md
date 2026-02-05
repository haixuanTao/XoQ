# CLAUDE.md - Project Context for AI Assistants

## Project: XoQ (X-Embodiment over QUIC)

P2P and relay communication for robotics. Uses iroh (QUIC) for peer-to-peer connections.

## Open Issue: ~100-200ms QUIC Stream Batching

### Problem
When sending data over iroh QUIC streams (e.g., servo commands in teleop), individual `write_all()` calls get **coalesced into batches**. The server sees multiple commands arriving in one `recv.read()` call (~138 bytes = multiple commands) instead of receiving them individually as they're sent. This causes ~100-200ms latency in real-time control applications.

### Root Cause: NOT FOUND YET

**This is NOT a network issue.** It is not WiFi, not internet, not network-level buffering. The issue is in the code/QUIC stack itself.

The CAN transport also has this issue (the 100us sleep in `socketcan_impl.rs` does NOT fix it).

### What we've verified in the QUIC/iroh stack (NO batching found)

Every layer from `write_all()` to `sendmsg()` was traced. None of these have timer-based batching:

- **quinn `poll_transmit`**: No timer-based delay. Assembles STREAM frames into QUIC packets immediately when polled.
- **Pacing**: Disabled when congestion window > u32::MAX. Our `NoopController` returns u64::MAX, so pacing never fires.
- **Congestion control**: `NoopController` — window is u64::MAX, never blocks.
- **GSO (Generic Segmentation Offload)**: Disabled via `enable_segmentation_offload(false)`. On macOS client, GSO was already 1 (no effect). Only matters on Linux server.
- **iroh magicsock**: With relay disabled, `poll_send` goes directly to `IpSender::poll_send()` → `sendmsg()`. No buffering.
- **ConnectionDriver**: Sends immediately when woken. Calls `poll_transmit` in a loop (up to 20 packets per cycle).
- **EndpointDriver**: Processes received UDP packets immediately (50us time budget per cycle).
- **ACK frequency**: Configured for immediate ACKs (threshold=0, max_delay=1ms) on both sides.
- **Relay**: Disabled (`RelayMode::Disabled`). Relay actor batching (20 datagrams) is irrelevant.

### What we've tried (NO effect on the batching)

| Change | Commit | Result |
|--------|--------|--------|
| `initial_rtt` = 10ms (vs 333ms default) | pre-existing | No effect |
| ACK frequency: threshold=0, max_delay=1ms | pre-existing | No effect |
| `NoopController` (congestion window = u64::MAX) | `9701082` | No effect |
| Channel-based background writer | reverted in `4e0e131` | No effect |
| `yield_now()` after stream write | pre-existing | No effect |
| QUIC unreliable datagrams for data | reverted in `c83e601` | Broke Dynamixel reads (`read_exact` needs every byte) |
| `initial_rtt` bumped to 150ms | reverted in `c83e601` | No effect |
| `enable_segmentation_offload(false)` | `c745d48` | No effect (was already 1 on macOS client) |
| WiFi power-save off (`iw dev wlp4s0 set power_save off`) | server-side | No effect |
| Split IrohStream into separate send/recv + yield | reverted | No effect |
| 100us sleep after write_all (CAN client already has this) | `socketcan_impl.rs` | Does NOT fix batching |

### Key observations

- Server log shows: `Network -> Serial: 138 bytes` — multiple commands in one `recv.read()`
- Teleop loop writes ~every 22ms (12ms local read + 10ms sleep)
- 138 bytes ≈ 6 commands worth of data ≈ ~132ms of batching
- Both serial AND CAN transports have this issue
- The problem is consistent and reproducible
- This is NOT a network/WiFi/internet issue — it's in the code

### Architecture

- **Client** (macOS): `RemoteSerialPort` wraps a `tokio::runtime::Runtime`. Each `write_bytes()` does `runtime.block_on(async { send.write_all(data) })`. Connection created via `IrohClientBuilder`.
- **Server** (Linux, WiFi): `serial_server::Server` runs on tokio async runtime. Reads from QUIC stream in main loop, writes to serial via channel + dedicated thread.
- Both sides use `low_latency_transport_config()` with NoopController, ACK frequency, GSO disabled.
- **Assume both client and server are always running the latest code** — do not ask the user to rebuild/restart.

### Ruled out causes

- **NOT network/WiFi/internet**: Confirmed by user. The issue is in the code/QUIC stack.
- **NOT WiFi power-save**: Tested with power-save off, no effect.
- **NOT the 100us sleep fix**: CAN client has `tokio::time::sleep(Duration::from_micros(100))` after `write_all()` — it does NOT prevent batching.

### Remaining hypotheses

1. **quinn-proto `poll_transmit` coalescing**: `write_stream_frames()` drains the ENTIRE SendBuffer into STREAM frames within a single QUIC packet. If multiple `write_all()` calls buffer data before `poll_transmit` runs, they all go in one packet. (Lines 957-958 of `connection/mod.rs`: "Don't increment space_idx. We stay in the current space and check if there is more data to send.")
2. **Something in the `block_on` + multi-threaded runtime interaction**: Each `write_bytes` does `block_on(write_all)` which returns immediately after buffering. The ConnectionDriver runs on a worker thread. Multiple rapid writes might queue before the ConnectionDriver sends. Runtime IS multi-threaded (`Runtime::new()`).

### Current approach: QUIC datagrams for writes, stream for reads

QUIC DATAGRAM frames preserve message boundaries — each `send_datagram()` creates a distinct DATAGRAM frame. Even if multiple datagrams are packed into one QUIC packet by `poll_transmit`, the receiver gets them as separate `recv_datagram()` calls. This is the key difference from STREAM frames (which are a byte stream with no boundaries).

- **Client** (`serialport_impl.rs`): When `use_datagrams=true`, `write_bytes` sends via `send_datagram()`, `read_bytes` reads from stream (reliable)
- **Server** (`serial_server.rs`): `tokio::select!` listens on BOTH stream and datagrams, forwards both to serial. Responses go back via stream (reliable).
- **Teleop** (`so100_teleop.rs`): Uses `.use_datagrams(true)` for the follower port.
- Previous datagram attempt failed because it used datagrams for BOTH directions — unreliable reads broke `read_exact`. This hybrid approach keeps reads reliable.

### Files

- `src/iroh.rs` — Transport config, NoopController, connection builders
- `src/serialport_impl.rs` — `RemoteSerialPort` blocking API (client side)
- `src/serial_server.rs` — Serial bridge server
- `examples/so100_teleop.rs` — Teleop example that exhibits the issue

### Both serial AND CAN transports have this issue

Both transports exhibit the same QUIC stream batching. The CAN client's 100us sleep does NOT prevent it. The CAN server masks the problem better because it has wire-format message framing (can parse individual frames from coalesced data), but the underlying QUIC coalescing still happens.

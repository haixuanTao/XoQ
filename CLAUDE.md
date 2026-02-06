# CLAUDE.md - Project Context for AI Assistants

## Project: XoQ (X-Embodiment over QUIC)

P2P and relay communication for robotics. Uses iroh (QUIC) for peer-to-peer connections and MoQ for relay-based pub/sub.

## Resolved: Latency Spikes Were WiFi, Not QUIC

**GitHub issue:** https://github.com/n0-computer/iroh/issues/3915

### Root Cause: WiFi Latency

The ~90ms latency spikes were caused by **WiFi**, not QUIC coalescing. Raw ICMP ping over WiFi showed identical spikes (min 3ms, max 94ms). WiFi has inherent latency variance due to CSMA/CA contention and beacon intervals.

### Solution: Use Ethernet

With Ethernet, latency is consistent:
- Min: 1.4ms, Max: 10.5ms, Avg: 2.0ms, P99: 5.2ms
- **Zero spikes >50ms**

### Important: Don't Use Datagrams Mode

`use_datagrams(true)` causes connection issues where the server waits indefinitely for stream. **Use streams mode** (`use_datagrams(false)`) for reliable operation.

### Files

- `src/iroh.rs` — Transport config, NoopController, connection builders
- `src/serialport_impl.rs` — `RemoteSerialPort` blocking API (client side)
- `src/serial_server.rs` — Serial bridge server
- `examples/so100_teleop.rs` — Teleop example (uses streams mode)
- `examples/iroh_latency_test.rs` — Latency/jitter measurement tool

## MoQ Relay

### cdn.moq.dev Limitation

cdn.moq.dev does NOT forward announcements between separate WebTransport sessions. Cross-session pub/sub is impossible through this relay.

### Solution: Self-hosted moq-relay

Run your own relay from https://github.com/kixelated/moq-rs:

```bash
# Build
cargo build --release -p moq-relay

# Run (with self-signed TLS)
moq-relay --server-bind 0.0.0.0:4443 --tls-generate localhost --auth-public anon
```

### Usage

```rust
// Publisher
let (_pub, mut track) = MoqBuilder::new()
    .relay("https://your-relay:4443")
    .path("anon/my-channel")
    .disable_tls_verify()  // for self-signed certs
    .connect_publisher_with_track("video")
    .await?;
track.write_str("hello");

// Subscriber (separate process)
let mut sub = MoqBuilder::new()
    .relay("https://your-relay:4443")
    .path("anon/my-channel")
    .disable_tls_verify()
    .connect_subscriber()
    .await?;
let mut reader = sub.subscribe_track("video").await?.unwrap();
let data = reader.read_string().await?;
```

### Files

- `src/moq.rs` — MoqBuilder, MoqPublisher, MoqSubscriber, MoqStream
- `examples/moq_test.rs` — Simple pub/sub test (`pub`, `sub` modes)

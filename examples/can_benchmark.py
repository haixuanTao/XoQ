#!/usr/bin/env python3
"""CAN bridge benchmark client (Python) - measures latency and throughput.

Connects to a running can_server and benchmarks query/response performance.
Uses Damiao MIT zero-torque command by default to safely poll motor positions.

Usage: python can_benchmark.py <server-endpoint-id> [options]

Options:
  --count N       Iterations (default: 100)
  --interval MS   Delay between iterations in ms (default: 20)
  --timeout MS    Read timeout in ms (default: 500)
  --can-id HEX    CAN ID, 0 = cycle 1-8 (default: 0)
  --motors N      Number of motors to cycle (default: 8)
  --mode MODE     rtt | write-only | read-only (default: rtt)
  --warmup N      Warmup iterations (default: 5)

Examples:
  python can_benchmark.py <id>                    # Query all 8 motors
  python can_benchmark.py <id> --can-id 0x01      # Query motor 1 only
  python can_benchmark.py <id> --count 500 --interval 10
"""

import argparse
import signal
import statistics
import sys
import time

import xoq_can

# Damiao MIT zero-torque: p_des=0, v_des=0, kp=0, kd=0, t_ff=0
MIT_ZERO_TORQUE = [0x80, 0x00, 0x80, 0x00, 0x00, 0x00, 0x08, 0x00]

running = True


def signal_handler(sig, frame):
    global running
    running = False


signal.signal(signal.SIGINT, signal_handler)


def resolve_can_id(args, iteration):
    if args.can_id != 0:
        return args.can_id
    return ((iteration - 1) % args.motors) + 1


def drain_stale(bus, timeout_s=0.05):
    drained = 0
    while True:
        msg = bus.recv(timeout=timeout_s)
        if msg is None:
            break
        drained += 1
    if drained > 0:
        print(f"Drained {drained} stale frames")


def print_stats(label, times_ms):
    if not times_ms:
        return
    s = sorted(times_ms)
    n = len(s)
    avg = statistics.mean(s)
    med = statistics.median(s)
    p95 = s[min(int(n * 0.95), n - 1)]
    p99 = s[min(int(n * 0.99), n - 1)]
    sd = statistics.stdev(s) if n > 1 else 0.0

    print(f"--- {label} ---")
    print(f"  Min:    {s[0]:>8.2f}ms")
    print(f"  Max:    {s[-1]:>8.2f}ms")
    print(f"  Avg:    {avg:>8.2f}ms")
    print(f"  Median: {med:>8.2f}ms")
    print(f"  P95:    {p95:>8.2f}ms")
    print(f"  P99:    {p99:>8.2f}ms")
    print(f"  StdDev: {sd:>8.2f}ms")


def print_histogram(times_ms):
    n = len(times_ms)
    buckets = [
        (1.0, "<= 1ms"),
        (2.0, "<= 2ms"),
        (5.0, "<= 5ms"),
        (10.0, "<= 10ms"),
        (20.0, "<= 20ms"),
        (50.0, "<= 50ms"),
        (100.0, "<= 100ms"),
    ]
    print("Histogram:")
    for limit, label in buckets:
        count = sum(1 for t in times_ms if t <= limit)
        print(f"  {label:>10}: {count:>5} ({count / n * 100:>5.1f}%)")
    over = sum(1 for t in times_ms if t > 100.0)
    print(f"  {'> 100ms':>10}: {over:>5} ({over / n * 100:>5.1f}%)")


def run_rtt(bus, args):
    drain_stale(bus)

    if args.warmup > 0:
        print(f"Warming up ({args.warmup} iterations)...")
        for w in range(args.warmup):
            cid = resolve_can_id(args, w + 1)
            msg = xoq_can.Message(arbitration_id=cid, data=MIT_ZERO_TORQUE)
            bus.send(msg)
            bus.recv(timeout=args.timeout / 1000.0)
            time.sleep(args.interval / 1000.0)
        drain_stale(bus)
        print("Warmup complete.\n")

    print("Starting RTT benchmark...\n")

    write_times = []
    read_times = []
    rtts = []
    timeouts = 0
    write_errors = 0
    read_errors = 0

    test_start = time.perf_counter()

    for i in range(1, args.count + 1):
        if not running:
            print(f"\nInterrupted at iteration {i}")
            break

        cid = resolve_can_id(args, i)
        msg = xoq_can.Message(arbitration_id=cid, data=MIT_ZERO_TORQUE)

        t0 = time.perf_counter()

        # Write
        try:
            bus.send(msg)
        except Exception as e:
            write_errors += 1
            print(f"  [{i}] Write error: {e}")
            time.sleep(args.interval / 1000.0)
            continue

        t1 = time.perf_counter()

        # Read
        try:
            resp = bus.recv(timeout=args.timeout / 1000.0)
        except Exception as e:
            read_errors += 1
            wt = (t1 - t0) * 1000
            write_times.append(wt)
            print(f"  [{i}] Read error: {e} (write={wt:.2f}ms)")
            continue

        t2 = time.perf_counter()

        wt = (t1 - t0) * 1000
        write_times.append(wt)

        if resp is None:
            timeouts += 1
            print(f"  [{i}] TIMEOUT (write={wt:.2f}ms)")
        else:
            rt = (t2 - t1) * 1000
            rtt = (t2 - t0) * 1000
            read_times.append(rt)
            rtts.append(rtt)

            if rtt > 50:
                print(f"  [{i}] RTT={rtt:.2f}ms (write={wt:.2f}ms, read={rt:.2f}ms) *** SLOW")
            elif i % 10 == 0 or i == args.count:
                print(f"  [{i}] RTT={rtt:.2f}ms (write={wt:.2f}ms, read={rt:.2f}ms)")

        elapsed = time.perf_counter() - t0
        sleep_time = args.interval / 1000.0 - elapsed
        if sleep_time > 0:
            time.sleep(sleep_time)

    total = time.perf_counter() - test_start

    print()
    print("=" * 60)
    print("=== RTT Benchmark Results (Python) ===")
    print()
    print(
        f"Iterations: {args.count} sent, {len(rtts)} successful, "
        f"{timeouts} timeouts, {write_errors} write errors, {read_errors} read errors"
    )
    print(f"Total time: {total:.2f}s")
    print()

    if rtts:
        print_stats("Round-trip latency", rtts)
        print()
        print_histogram(rtts)

    if write_times:
        print()
        print_stats("Write latency", write_times)

    if read_times:
        print()
        print_stats("Read latency", read_times)


def run_write_only(bus, args):
    if args.warmup > 0:
        print(f"Warming up ({args.warmup} iterations)...")
        for w in range(args.warmup):
            cid = resolve_can_id(args, w + 1)
            msg = xoq_can.Message(arbitration_id=cid, data=MIT_ZERO_TORQUE)
            bus.send(msg)
            time.sleep(args.interval / 1000.0)
        print("Warmup complete.\n")

    print("Starting write-only benchmark...\n")

    write_times = []
    write_errors = 0

    test_start = time.perf_counter()

    for i in range(1, args.count + 1):
        if not running:
            print(f"\nInterrupted at iteration {i}")
            break

        cid = resolve_can_id(args, i)
        msg = xoq_can.Message(arbitration_id=cid, data=MIT_ZERO_TORQUE)

        t0 = time.perf_counter()
        try:
            bus.send(msg)
            wt = (time.perf_counter() - t0) * 1000
            write_times.append(wt)

            if wt > 50:
                print(f"  [{i}] write={wt:.2f}ms *** SLOW")
            elif i % 10 == 0 or i == args.count:
                print(f"  [{i}] write={wt:.2f}ms")
        except Exception as e:
            write_errors += 1
            print(f"  [{i}] Write error: {e}")

        elapsed = time.perf_counter() - t0
        sleep_time = args.interval / 1000.0 - elapsed
        if sleep_time > 0:
            time.sleep(sleep_time)

    total = time.perf_counter() - test_start

    print()
    print("=" * 60)
    print("=== Write-Only Benchmark Results (Python) ===")
    print()
    print(f"Iterations: {args.count} sent, {len(write_times)} successful, {write_errors} errors")
    print(f"Total time: {total:.2f}s")
    print()

    if write_times:
        print_stats("Write latency", write_times)
        print()
        print_histogram(write_times)


def run_read_only(bus, args):
    drain_stale(bus)

    print(f"Starting read-only benchmark (waiting for {args.count} frames)...\n")

    read_times = []
    timeouts = 0
    read_errors = 0

    test_start = time.perf_counter()

    while len(read_times) < args.count:
        if not running:
            print(f"\nInterrupted after {len(read_times)} frames")
            break

        t0 = time.perf_counter()
        try:
            msg = bus.recv(timeout=args.timeout / 1000.0)
        except Exception as e:
            read_errors += 1
            print(f"  Read error: {e}")
            continue

        if msg is None:
            timeouts += 1
            continue

        rt = (time.perf_counter() - t0) * 1000
        read_times.append(rt)
        n = len(read_times)

        if rt > 50:
            print(
                f"  [{n}] read={rt:.2f}ms ID=0x{msg.arbitration_id:03x} "
                f"len={len(msg.data)} *** SLOW"
            )
        elif n % 10 == 0 or n == args.count:
            print(f"  [{n}] read={rt:.2f}ms ID=0x{msg.arbitration_id:03x} len={len(msg.data)}")

    total = time.perf_counter() - test_start

    print()
    print("=" * 60)
    print("=== Read-Only Benchmark Results (Python) ===")
    print()
    print(f"Frames: {len(read_times)} received, {timeouts} timeouts, {read_errors} errors")
    print(f"Total time: {total:.2f}s")
    if read_times:
        print(f"Throughput: {len(read_times) / total:.1f} frames/sec")
    print()

    if read_times:
        print_stats("Read latency", read_times)
        print()
        print_histogram(read_times)


def main():
    parser = argparse.ArgumentParser(description="CAN bridge benchmark (Python)")
    parser.add_argument("server_id", help="Server endpoint ID")
    parser.add_argument("--count", type=int, default=100, help="Iterations (default: 100)")
    parser.add_argument("--interval", type=float, default=20, help="Delay between iterations in ms (default: 20)")
    parser.add_argument("--timeout", type=float, default=500, help="Read timeout in ms (default: 500)")
    parser.add_argument("--can-id", type=lambda x: int(x, 0), default=0, dest="can_id", help="CAN ID, 0=cycle (default: 0)")
    parser.add_argument("--motors", type=int, default=8, help="Motors to cycle (default: 8)")
    parser.add_argument("--mode", choices=["rtt", "write-only", "read-only"], default="rtt", help="Benchmark mode (default: rtt)")
    parser.add_argument("--warmup", type=int, default=5, help="Warmup iterations (default: 5)")
    args = parser.parse_args()

    print("=== CAN Bridge Benchmark (Python) ===")
    print(f"Server:   {args.server_id}")
    print(f"Mode:     {args.mode}")
    print(f"Count:    {args.count}")
    print(f"Interval: {args.interval}ms")
    print(f"Timeout:  {args.timeout}ms")
    if args.can_id == 0:
        print(f"CAN ID:   cycle 0x001-0x{args.motors:03x} ({args.motors} motors)")
    else:
        print(f"CAN ID:   0x{args.can_id:03x}")
    payload_hex = " ".join(f"{b:02x}" for b in MIT_ZERO_TORQUE)
    print(f"Payload:  [{payload_hex}] (MIT zero-torque query)")
    print(f"Warmup:   {args.warmup}")
    print()

    print(f"Connecting to CAN bridge: {args.server_id}")
    bus = xoq_can.Bus(channel=args.server_id, timeout=args.timeout / 1000.0)
    print("Connected!")
    print()

    if args.mode == "rtt":
        run_rtt(bus, args)
    elif args.mode == "write-only":
        run_write_only(bus, args)
    elif args.mode == "read-only":
        run_read_only(bus, args)

    bus.shutdown()


if __name__ == "__main__":
    main()

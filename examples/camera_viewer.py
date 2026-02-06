#!/usr/bin/env python3
"""
Camera viewer: connects to xoq camera server and displays video.

Setup:
    pip install opencv-python numpy

    # Build and install xoq cv2 extension
    cd packages/cv2 && maturin develop --features videotoolbox --release

    # Install xoq meta-package (patches import cv2 automatically)
    cd packages/xoq && pip install -e .

    # Start camera server (in another terminal)
    cargo run --example camera_server --features iroh,vtenc -- 0 --h264

Usage:
    python examples/camera_viewer.py <server-id>        # auto-detect (iroh)
    python examples/camera_viewer.py anon/camera-0      # auto-detect (MoQ)
    python examples/camera_viewer.py <source> --moq     # force MoQ
    python examples/camera_viewer.py <source> --iroh    # force iroh
"""

import sys
import time

import cv2

MAX_RETRIES = 3
RETRY_DELAY = 1.0  # seconds


def main():
    if len(sys.argv) < 2:
        print("Usage: python camera_viewer.py <source>")
        print()
        print("  source: iroh server ID or MoQ path (auto-detected)")
        print("  --moq   force MoQ relay transport")
        print("  --iroh  force iroh P2P transport")
        sys.exit(1)

    source = sys.argv[1]

    # Determine transport: explicit flag > auto-detect
    if "--moq" in sys.argv:
        transport = "moq"
    elif "--iroh" in sys.argv:
        transport = "iroh"
    else:
        transport = None  # auto-detect

    transport_label = transport or ("MoQ" if "/" in source else "iroh")
    print(f"Connecting to {source} ({transport_label})...")

    cap = None
    for attempt in range(1, MAX_RETRIES + 1):
        try:
            cap = cv2.VideoCapture(source, transport=transport)
            break
        except RuntimeError as e:
            if attempt < MAX_RETRIES:
                print(f"  Connection attempt {attempt} failed: {e}")
                print(f"  Retrying in {RETRY_DELAY}s...")
                time.sleep(RETRY_DELAY)
            else:
                print(f"Failed to connect after {MAX_RETRIES} attempts: {e}")
                sys.exit(1)

    if not cap.isOpened():
        print("Failed to connect!")
        sys.exit(1)

    print("Connected! Press 'q' to quit.")

    frame_count = 0
    fps_start = time.time()
    fps_count = 0

    while True:
        ret, frame = cap.read()
        if not ret:
            print("Failed to read frame")
            break

        cv2.imshow("XOQ Remote Camera", frame)
        if cv2.waitKey(1) & 0xFF == ord("q"):
            break

        frame_count += 1
        fps_count += 1
        now = time.time()
        elapsed = now - fps_start
        if elapsed >= 1.0:
            fps = fps_count / elapsed
            h, w = frame.shape[:2]
            print(f"  {fps:.1f} FPS ({w}x{h}, {frame_count} frames)")
            fps_start = now
            fps_count = 0

    cap.release()
    cv2.destroyAllWindows()
    print(f"Done. {frame_count} frames received.")


if __name__ == "__main__":
    main()

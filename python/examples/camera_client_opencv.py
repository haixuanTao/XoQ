#!/usr/bin/env python3
"""Camera client example using OpenCV-compatible interface.

This example shows how to use xoq.VideoCapture as a drop-in
replacement for cv2.VideoCapture to receive frames from a remote
camera server.

Supports both iroh (P2P) and MoQ (relay) transports with auto-detection:
- If source contains "/", uses MoQ (e.g., "anon/my-camera")
- Otherwise, uses iroh P2P (endpoint ID)

Usage:
    # Auto-detect transport based on source format
    python camera_client_opencv.py <source>

    # Explicit transport selection
    python camera_client_opencv.py --moq anon/my-camera
    python camera_client_opencv.py --iroh <server-endpoint-id>

Examples:
    python camera_client_opencv.py anon/my-camera        # MoQ (auto-detected)
    python camera_client_opencv.py abc123def456...      # iroh (auto-detected)
    python camera_client_opencv.py --moq anon/my-camera  # MoQ (explicit)

Requirements:
    pip install opencv-python
"""

import sys

import cv2
import xoq


def main():
    if len(sys.argv) < 2:
        print("Usage: python camera_client_opencv.py [--moq|--iroh] <source>")
        print("  source: MoQ path (e.g., anon/my-camera) or iroh endpoint ID")
        print("  --moq:  Force MoQ transport")
        print("  --iroh: Force iroh P2P transport")
        sys.exit(1)

    # Parse arguments
    transport = None
    source = None

    args = sys.argv[1:]
    if args[0] == "--moq":
        transport = "moq"
        source = args[1] if len(args) > 1 else None
    elif args[0] == "--iroh":
        transport = "iroh"
        source = args[1] if len(args) > 1 else None
    else:
        source = args[0]

    if not source:
        print("Error: No source specified")
        sys.exit(1)

    # Determine transport for display
    if transport:
        transport_name = transport.upper()
    elif "/" in source:
        transport_name = "MoQ (auto-detected)"
    else:
        transport_name = "iroh (auto-detected)"

    print(f"Connecting via {transport_name}: {source}")

    # Connect to remote camera server
    cap = xoq.VideoCapture(source, transport=transport)

    if not cap.isOpened():
        print("Failed to connect to camera server")
        sys.exit(1)

    print("Connected! Press 'q' to quit.\n")

    while True:
        # Standard OpenCV read() call
        ret, frame = cap.read()

        if not ret:
            break

        # Get frame dimensions from numpy array shape
        height, width = frame.shape[:2]

        # Display frame info
        cv2.putText(
            frame,
            f"Remote Camera: {width}x{height}",
            (10, 30),
            cv2.FONT_HERSHEY_SIMPLEX,
            0.7,
            (0, 255, 0),
            2,
        )

        # Show the frame
        cv2.imshow("Remote Camera", frame)

        # Press 'q' to quit
        if cv2.waitKey(1) & 0xFF == ord("q"):
            break

    # Clean up
    cap.release()
    cv2.destroyAllWindows()


if __name__ == "__main__":
    main()

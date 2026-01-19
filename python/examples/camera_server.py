#!/usr/bin/env python3
"""Camera server example - streams local camera over P2P.

This example shows how to use xoq.CameraServer to stream a local
camera to remote clients over iroh P2P.

Usage:
    python camera_server.py [camera_index]
"""

import sys

import xoq


def main():
    camera_index = int(sys.argv[1]) if len(sys.argv) > 1 else 0

    print(f"Starting camera server (camera {camera_index})...")

    # Create camera server with default settings (640x480, 30fps)
    server = xoq.CameraServer(
        camera_index=camera_index,
        width=640,
        height=480,
        fps=30,
    )

    print(f"\nCamera server started!")
    print(f"Server ID: {server.id()}")
    print(f"\nShare this ID with clients to connect.")
    print(f"Press Ctrl+C to stop.\n")

    try:
        # Run forever, handling client connections
        server.run()
    except KeyboardInterrupt:
        print("\nServer stopped")


if __name__ == "__main__":
    main()

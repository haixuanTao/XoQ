#!/usr/bin/env python3
"""Camera client example using OpenCV-compatible interface.

This example shows how to use xoq.VideoCapture as a drop-in
replacement for cv2.VideoCapture to receive frames from a remote
camera server.

Usage:
    python camera_client_opencv.py <server-id>

Requirements:
    pip install opencv-python
"""

import sys

import cv2
import xoq


def main():
    if len(sys.argv) < 2:
        print("Usage: python camera_client_opencv.py <server-id>")
        sys.exit(1)

    server_id = sys.argv[1]
    print(f"Connecting to camera server: {server_id}")

    # Connect to remote camera server
    cap = xoq.VideoCapture(server_id)

    if not cap.isOpened():
        print("Failed to connect to camera server")
        sys.exit(1)

    print("Connected! Press 'q' to quit.\n")

    while True:
        # Standard OpenCV read() call
        ret, frame = cap.read()

        if not ret:
            print("Failed to read frame")
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

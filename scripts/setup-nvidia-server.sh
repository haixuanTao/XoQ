#!/usr/bin/env bash
# setup.sh — Set up a fresh Ubuntu NVIDIA server for building and running xoq binaries.
#
# Usage:
#   bash setup.sh              # Install deps + clone + build with realsense feature
#   bash setup.sh --skip-build # Install deps only
#   bash setup.sh --all-features # Build with all Linux features
#
# Target: Ubuntu 24.04 (noble) with NVIDIA driver already installed.

set -euo pipefail

# ============================================================================
# Parse flags
# ============================================================================
SKIP_BUILD=false
ALL_FEATURES=false
REPO_URL="https://github.com/haixuanTao/wser"
REPO_DIR="$HOME/wser"

for arg in "$@"; do
    case "$arg" in
        --skip-build) SKIP_BUILD=true ;;
        --all-features) ALL_FEATURES=true ;;
        --help|-h)
            echo "Usage: bash setup.sh [--skip-build] [--all-features]"
            echo "  --skip-build    Only install dependencies, don't clone/build"
            echo "  --all-features  Build with iroh,camera,nvenc,realsense,can,audio,serial"
            exit 0
            ;;
        *) echo "Unknown flag: $arg"; exit 1 ;;
    esac
done

# ============================================================================
# Helpers
# ============================================================================
section() {
    echo ""
    echo "========================================"
    echo "  $1"
    echo "========================================"
    echo ""
}

check_cmd() {
    command -v "$1" &>/dev/null
}

# Prompt for sudo password once upfront
sudo -v

# ============================================================================
# 1. System packages
# ============================================================================
section "System packages"

PACKAGES=(
    build-essential
    pkg-config
    libssl-dev
    libasound2-dev
    libv4l-dev
    can-utils
    clang
    curl
)

MISSING=()
for pkg in "${PACKAGES[@]}"; do
    if ! dpkg -s "$pkg" &>/dev/null; then
        MISSING+=("$pkg")
    fi
done

if [ ${#MISSING[@]} -gt 0 ]; then
    echo "Installing: ${MISSING[*]}"
    sudo apt-get update -qq
    sudo apt-get install -y "${MISSING[@]}"
else
    echo "All system packages already installed."
fi

# ============================================================================
# 2. CUDA toolkit (provides nvcc)
# ============================================================================
section "CUDA toolkit"

if check_cmd nvcc; then
    echo "nvcc already installed: $(nvcc --version | grep release)"
else
    echo "Installing nvidia-cuda-toolkit (provides nvcc)..."
    sudo apt-get update -qq
    sudo apt-get install -y nvidia-cuda-toolkit
    echo "nvcc installed: $(nvcc --version | grep release)"
fi

# Verify NVENC libraries exist (installed with NVIDIA driver)
NVENC_FOUND=false
for dir in /usr/lib/x86_64-linux-gnu /usr/lib64 /usr/local/cuda/lib64; do
    if [ -f "$dir/libnvidia-encode.so" ] || ls "$dir"/libnvidia-encode.so.* &>/dev/null 2>&1; then
        NVENC_FOUND=true
        echo "NVENC library found in $dir"
        break
    fi
done
if [ "$NVENC_FOUND" = false ]; then
    echo "WARNING: libnvidia-encode.so not found. NVENC encoding may fail."
    echo "  This library should come with the NVIDIA driver (nvidia-driver-xxx)."
fi

# ============================================================================
# 3. Intel RealSense SDK
# ============================================================================
section "Intel RealSense SDK"

if dpkg -s librealsense2-dev &>/dev/null; then
    echo "librealsense2-dev already installed."
else
    echo "Adding Intel RealSense apt repository..."

    # Install prerequisites for the repo
    sudo apt-get install -y apt-transport-https software-properties-common

    # Register the server's public key
    sudo mkdir -p /etc/apt/keyrings
    curl -sSf https://librealsense.intel.com/Debian/librealsense.pgp | sudo tee /etc/apt/keyrings/librealsense.pgp >/dev/null

    # Add the repository
    echo "deb [signed-by=/etc/apt/keyrings/librealsense.pgp] https://librealsense.intel.com/Debian/apt-repo $(lsb_release -cs) main" | \
        sudo tee /etc/apt/sources.list.d/librealsense.list

    sudo apt-get update -qq
    sudo apt-get install -y librealsense2-dev librealsense2-utils
    echo "RealSense SDK installed."
fi

# ============================================================================
# 4. Rust (via rustup)
# ============================================================================
section "Rust toolchain"

if check_cmd rustc; then
    echo "Rust already installed: $(rustc --version)"
else
    echo "Installing Rust via rustup..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    # shellcheck source=/dev/null
    source "$HOME/.cargo/env"
    echo "Rust installed: $(rustc --version)"
fi

# Ensure cargo is in PATH for the rest of the script
if ! check_cmd cargo; then
    # shellcheck source=/dev/null
    source "$HOME/.cargo/env"
fi

# ============================================================================
# 5. Clone repo
# ============================================================================
if [ "$SKIP_BUILD" = false ]; then
    section "Clone repository"

    if [ -d "$REPO_DIR/.git" ]; then
        echo "Repository already exists at $REPO_DIR, pulling latest..."
        git -C "$REPO_DIR" pull --ff-only || echo "Pull failed (maybe dirty), skipping."
    else
        echo "Cloning $REPO_URL into $REPO_DIR..."
        git clone "$REPO_URL" "$REPO_DIR"
    fi

    # ========================================================================
    # 6. Build
    # ========================================================================
    section "Build"

    cd "$REPO_DIR"

    if [ "$ALL_FEATURES" = true ]; then
        FEATURES="iroh,camera,nvenc,realsense,can,audio,serial"
    else
        FEATURES="realsense"
    fi

    echo "Building with features: $FEATURES"
    cargo build --release --features "$FEATURES"
    echo "Build succeeded."

    # ========================================================================
    # 7. Verify
    # ========================================================================
    section "Verify"

    echo "Testing realsense-server --help:"
    ./target/release/realsense-server --help
    echo ""

    echo "Checking for RealSense devices:"
    rs-enumerate-devices --compact || echo "(no devices found or rs-enumerate-devices not available)"
    echo ""

    echo "All done! Binary at: $REPO_DIR/target/release/realsense-server"
fi

section "Setup complete"
echo "Summary:"
echo "  nvcc:              $(nvcc --version 2>/dev/null | grep release || echo 'not found')"
echo "  rustc:             $(rustc --version 2>/dev/null || echo 'not found')"
echo "  librealsense2-dev: $(dpkg -s librealsense2-dev 2>/dev/null | grep Version || echo 'not found')"
if [ "$SKIP_BUILD" = false ]; then
    echo "  realsense-server:  $REPO_DIR/target/release/realsense-server"
fi

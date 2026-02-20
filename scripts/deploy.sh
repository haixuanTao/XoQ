#!/usr/bin/env bash
# deploy.sh — Auto-discover hardware, build binaries, generate services, and deploy XoQ.
#
# Works on both Linux (systemd) and macOS (launchd).
#
# Usage:
#   scripts/deploy.sh              # Discover hardware → generate services → start
#   scripts/deploy.sh --build      # Also build binaries with correct feature flags
#   scripts/deploy.sh --boot       # Also enable start-on-boot (systemd enable / launchd RunAtLoad)
#   scripts/deploy.sh --dry-run    # Show what would happen without doing anything
#   scripts/deploy.sh --status     # Show status of all xoq services + machine.json
#   scripts/deploy.sh --uninstall  # Stop + disable + remove services (keeps keys)
#   scripts/deploy.sh --json       # Regenerate machine.json only

set -euo pipefail

# Save original args for re-exec after git pull
ORIGINAL_ARGS=("$@")

# ============================================================================
# Platform detection
# ============================================================================
OS="$(uname -s)"
case "$OS" in
    Linux)  PLATFORM="linux" ;;
    Darwin) PLATFORM="macos" ;;
    *)      echo "Unsupported OS: $OS"; exit 1 ;;
esac

# ============================================================================
# Configuration (overridable via environment)
# ============================================================================
XOQ_RELAY="${XOQ_RELAY:-https://cdn.1ms.ai}"
XOQ_CONFIG_DIR="${HOME}/.config/xoq"
XOQ_KEY_DIR="${XOQ_KEY_DIR:-${XOQ_CONFIG_DIR}/keys}"
XOQ_LAUNCHD_DIR="${HOME}/Library/LaunchAgents"    # Used with --boot
XOQ_AGENTS_DIR="${HOME}/.config/xoq/agents"        # Default (no boot)
XOQ_SYSTEMD_DIR="${HOME}/.config/systemd/user"

# Find project root (where Cargo.toml lives)
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# Find binaries: prefer XOQ_BIN_DIR env, then ./target/release, then PATH
if [ -n "${XOQ_BIN_DIR:-}" ]; then
    BIN_DIR="$XOQ_BIN_DIR"
elif [ -d "${PROJECT_ROOT}/target/release" ]; then
    BIN_DIR="${PROJECT_ROOT}/target/release"
else
    BIN_DIR=""
fi

DRY_RUN=false
DO_BUILD=false
ENABLE_BOOT=false
MODE="deploy"

# ============================================================================
# Parse arguments
# ============================================================================
while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run)    DRY_RUN=true; shift ;;
        --build)      DO_BUILD=true; shift ;;
        --boot)       ENABLE_BOOT=true; shift ;;
        --status)     MODE="status"; shift ;;
        --uninstall)  MODE="uninstall"; shift ;;
        --json)       MODE="json"; shift ;;
        --relay)      XOQ_RELAY="$2"; shift 2 ;;
        --bin-dir)    BIN_DIR="$2"; shift 2 ;;
        --help|-h)
            echo "Usage: $0 [--build] [--boot] [--dry-run] [--status] [--uninstall] [--json]"
            echo ""
            echo "Options:"
            echo "  --build       Build binaries with auto-detected feature flags before deploying"
            echo "  --boot        Enable start-on-boot (systemd enable / launchd RunAtLoad)"
            echo "  --dry-run     Show what would happen without doing anything"
            echo "  --status      Show status of all xoq services + machine.json"
            echo "  --uninstall   Stop + disable + remove services (keeps keys)"
            echo "  --json        Regenerate machine.json only"
            echo "  --relay URL   Override relay URL (default: ${XOQ_RELAY})"
            echo "  --bin-dir DIR Override binary directory"
            echo ""
            echo "Environment overrides: XOQ_RELAY, XOQ_BIN_DIR, XOQ_KEY_DIR"
            exit 0
            ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

# ============================================================================
# Machine ID
# ============================================================================
if [ -f /etc/machine-id ]; then
    # Linux: /etc/machine-id
    XOQ_MACHINE_ID="$(head -c 12 /etc/machine-id)"
elif [ "$PLATFORM" = "macos" ]; then
    # macOS: hardware UUID from IOKit
    XOQ_MACHINE_ID="$(ioreg -d2 -c IOPlatformExpertDevice | awk -F'"' '/IOPlatformUUID/{print $4}' | tr -d '-' | head -c 12 | tr '[:upper:]' '[:lower:]')"
fi

if [ -z "${XOQ_MACHINE_ID:-}" ]; then
    echo "[error] Cannot determine machine ID."
    exit 1
fi

# ============================================================================
# Ensure cargo and CUDA are in PATH
# ============================================================================
if [ -f "${HOME}/.cargo/env" ]; then
    # shellcheck source=/dev/null
    source "${HOME}/.cargo/env"
fi
if [ -d "/usr/local/cuda/bin" ]; then
    export PATH="/usr/local/cuda/bin:${PATH}"
fi

# ============================================================================
# Colors
# ============================================================================
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
NC='\033[0m'

info()  { echo -e "${BLUE}[info]${NC}  $*"; }
ok()    { echo -e "${GREEN}[ok]${NC}    $*"; }
warn()  { echo -e "${YELLOW}[warn]${NC}  $*"; }
err()   { echo -e "${RED}[error]${NC} $*"; }
header(){ echo -e "\n${BOLD}=== $* ===${NC}"; }

# ============================================================================
# Binary resolution
# ============================================================================
find_bin() {
    local name="$1"
    if [ -n "$BIN_DIR" ] && [ -x "${BIN_DIR}/${name}" ]; then
        echo "${BIN_DIR}/${name}"
    elif command -v "$name" &>/dev/null; then
        command -v "$name"
    else
        echo ""
    fi
}

# ============================================================================
# Hardware Discovery
# ============================================================================
discover_can() {
    # CAN is Linux-only (socketcan)
    [ "$PLATFORM" != "linux" ] && return
    local interfaces=()
    for iface in /sys/class/net/can*; do
        [ -e "$iface" ] || continue
        interfaces+=("$(basename "$iface")")
    done
    echo "${interfaces[*]:-}"
}

discover_realsense() {
    local serials=()

    # Method 1: rs-enumerate-devices (if librealsense2-utils is installed)
    if command -v rs-enumerate-devices &>/dev/null; then
        while IFS= read -r serial; do
            serials+=("$serial")
        done < <(rs-enumerate-devices --compact 2>/dev/null | grep -E '^\s+Serial Number' | grep -oE '[0-9]{6,}' || true)
    fi

    # Method 2: fallback to realsense-server binary (it logs connected devices on startup)
    # Run with a dummy serial so it exits immediately after printing device list
    if [ ${#serials[@]} -eq 0 ]; then
        local rs_bin
        rs_bin=$(find_bin realsense-server)
        if [ -n "$rs_bin" ]; then
            while IFS= read -r serial; do
                serials+=("$serial")
            done < <(timeout 3 "$rs_bin" --serial 0000000000 2>&1 | grep -oE '\(serial: [0-9]+\)' | grep -oE '[0-9]+' || true)
        fi
    fi

    echo "${serials[*]:-}"
}

discover_cameras() {
    if [ "$PLATFORM" = "linux" ]; then
        discover_v4l2_cameras
    else
        discover_macos_cameras
    fi
}

discover_v4l2_cameras() {
    # Find V4L2 capture devices, excluding RealSense cameras
    local cameras=()
    for dev in /dev/video*; do
        [ -e "$dev" ] || continue
        local idx="${dev#/dev/video}"

        # Check if device supports video capture
        if ! v4l2-ctl -d "$dev" --all 2>/dev/null | grep -q "Video Capture"; then
            continue
        fi

        # Exclude Intel RealSense devices (vendor 8086)
        local vendor
        vendor=$(udevadm info -n "$dev" 2>/dev/null | grep -oP 'ID_VENDOR_ID=\K.*' || true)
        if [ "$vendor" = "8086" ]; then
            continue
        fi

        cameras+=("$idx")
    done
    echo "${cameras[*]:-}"
}

discover_macos_cameras() {
    # On macOS, list AVFoundation capture devices by index
    # system_profiler gives us camera count; indices start at 0
    local count=0
    if command -v system_profiler &>/dev/null; then
        count=$(system_profiler SPCameraDataType 2>/dev/null | grep -c "Model ID:" || true)
    fi
    local cameras=()
    for (( i=0; i<count; i++ )); do
        cameras+=("$i")
    done
    echo "${cameras[*]:-}"
}

discover_audio() {
    if [ "$PLATFORM" = "linux" ]; then
        # Check for ALSA recording devices
        if command -v arecord &>/dev/null; then
            if arecord -l 2>/dev/null | grep -q "^card"; then
                echo "yes"
                return
            fi
        fi
    else
        # macOS always has audio input (built-in mic or external)
        if system_profiler SPAudioDataType 2>/dev/null | grep -q "Input"; then
            echo "yes"
            return
        fi
    fi
    echo ""
}

# ============================================================================
# Build
# ============================================================================
do_build() {
    local can_ifaces=($1)
    local rs_serials=($2)
    local cam_indices=($3)
    local has_audio="$4"

    # Pull latest changes from git
    header "Updating Source"
    if [ -d "${PROJECT_ROOT}/.git" ]; then
        local current_branch
        current_branch=$(git -C "${PROJECT_ROOT}" rev-parse --abbrev-ref HEAD 2>/dev/null || echo "unknown")
        local before_hash
        before_hash=$(git -C "${PROJECT_ROOT}" rev-parse HEAD 2>/dev/null || echo "unknown")

        if [ "$DRY_RUN" = true ]; then
            info "Would run: git -C ${PROJECT_ROOT} pull --ff-only"
        else
            git -C "${PROJECT_ROOT}" pull --ff-only || warn "git pull failed (dirty tree?), building with current code"
        fi

        local after_hash
        after_hash=$(git -C "${PROJECT_ROOT}" rev-parse HEAD 2>/dev/null || echo "unknown")
        if [ "$before_hash" = "$after_hash" ]; then
            ok "Already up to date (${current_branch} @ ${before_hash:0:8})"
        else
            ok "Updated ${current_branch}: ${before_hash:0:8} → ${after_hash:0:8}"
            # Re-exec with the updated script so new deploy logic takes effect
            info "Re-executing updated deploy script..."
            exec bash "${PROJECT_ROOT}/scripts/deploy.sh" "${ORIGINAL_ARGS[@]}"
        fi
    else
        warn "Not a git repository, skipping pull"
    fi

    header "Building Binaries"

    # Collect features needed
    local features=("iroh")
    local bins=()

    if [ ${#can_ifaces[@]} -gt 0 ] && [ -n "${can_ifaces[0]:-}" ]; then
        features+=("can")
        bins+=("can-server")
        bins+=("fake-can-server")
    fi

    if [ ${#rs_serials[@]} -gt 0 ] && [ -n "${rs_serials[0]:-}" ]; then
        features+=("realsense")
        bins+=("realsense-server")
    fi

    if [ ${#cam_indices[@]} -gt 0 ] && [ -n "${cam_indices[0]:-}" ]; then
        if [ "$PLATFORM" = "macos" ]; then
            features+=("vtenc")
        else
            features+=("camera")
        fi
        bins+=("camera-server")
    fi

    if [ "$has_audio" = "yes" ]; then
        if [ "$PLATFORM" = "macos" ]; then
            features+=("audio-macos")
        else
            features+=("audio")
        fi
        bins+=("audio-server")
    fi

    # Deduplicate features
    local unique_features
    unique_features=$(printf '%s\n' "${features[@]}" | sort -u | tr '\n' ',' | sed 's/,$//')

    # Build all needed binaries in one cargo invocation
    local cargo_args=(cargo build --release --manifest-path "${PROJECT_ROOT}/Cargo.toml" --features "$unique_features")
    for bin in "${bins[@]}"; do
        cargo_args+=(--bin "$bin")
    done

    info "Features: ${unique_features}"
    info "Binaries: ${bins[*]}"
    echo ""

    if [ "$DRY_RUN" = true ]; then
        info "Would run: ${cargo_args[*]}"
        return
    fi

    "${cargo_args[@]}"

    # Update BIN_DIR to point to freshly built binaries
    BIN_DIR="${PROJECT_ROOT}/target/release"
    ok "Build complete: ${BIN_DIR}"
}

# ============================================================================
# Status mode
# ============================================================================
do_status() {
    header "XoQ Service Status"
    echo "Machine ID: ${XOQ_MACHINE_ID}"
    echo "Platform:   ${PLATFORM}"
    echo ""

    if [ "$PLATFORM" = "linux" ]; then
        local units
        units=$(systemctl --user list-units --all 'xoq-*' --no-legend 2>/dev/null || true)
        if [ -z "$units" ]; then
            info "No xoq services found."
        else
            systemctl --user list-units --all 'xoq-*' --no-legend
        fi
        echo ""
        local target_status
        target_status=$(systemctl --user is-active xoq.target 2>/dev/null || echo "inactive")
        echo "xoq.target: ${target_status}"
    else
        # macOS: check launchd
        local agents
        agents=$(launchctl list 2>/dev/null | grep "com.xoq" || true)
        if [ -z "$agents" ]; then
            info "No xoq services found."
        else
            echo "$agents"
        fi
    fi

    echo ""
    if [ -f "${XOQ_CONFIG_DIR}/machine.json" ]; then
        header "machine.json"
        cat "${XOQ_CONFIG_DIR}/machine.json"
    else
        info "No machine.json found at ${XOQ_CONFIG_DIR}/machine.json"
    fi
}

# ============================================================================
# Uninstall mode
# ============================================================================
do_uninstall() {
    header "Uninstalling XoQ services"

    if [ "$PLATFORM" = "linux" ]; then
        do_uninstall_linux
    else
        do_uninstall_macos
    fi

    # Remove config (keep keys)
    rm -f "${XOQ_CONFIG_DIR}/env"
    rm -f "${XOQ_CONFIG_DIR}/machine.json"

    ok "Services removed. Keys preserved at ${XOQ_KEY_DIR}"
}

do_uninstall_linux() {
    info "Stopping xoq.target..."
    systemctl --user stop xoq.target 2>/dev/null || true

    local units=()
    for f in "${XOQ_SYSTEMD_DIR}"/xoq-*; do
        [ -f "$f" ] || continue
        units+=("$(basename "$f")")
    done
    [ -f "${XOQ_SYSTEMD_DIR}/xoq.target" ] && units+=("xoq.target")

    for unit in "${units[@]}"; do
        info "Disabling ${unit}..."
        systemctl --user disable "$unit" 2>/dev/null || true
        rm -f "${XOQ_SYSTEMD_DIR}/${unit}"
    done

    systemctl --user daemon-reload
}

do_uninstall_macos() {
    # Check both directories (agents dir for non-boot, LaunchAgents for boot)
    for dir in "${XOQ_AGENTS_DIR}" "${XOQ_LAUNCHD_DIR}"; do
        for plist in "${dir}"/com.xoq.*.plist; do
            [ -f "$plist" ] || continue
            local label
            label=$(basename "$plist" .plist)
            info "Unloading ${label}..."
            launchctl bootout "gui/$(id -u)/${label}" 2>/dev/null || true
            rm -f "$plist"
        done
    done
}

# ============================================================================
# Generate systemd units (Linux)
# ============================================================================
generate_env_file() {
    cat > "${XOQ_CONFIG_DIR}/env" <<EOF
XOQ_MACHINE_ID=${XOQ_MACHINE_ID}
XOQ_RELAY=${XOQ_RELAY}
XOQ_KEY_DIR=${XOQ_KEY_DIR}
XOQ_BIN_DIR=${BIN_DIR}
RUST_LOG=xoq=info,warn
EOF
}

generate_target() {
    cat > "${XOQ_SYSTEMD_DIR}/xoq.target" <<EOF
[Unit]
Description=XoQ Services
After=network-online.target

[Install]
WantedBy=default.target
EOF
}

generate_can_template() {
    cat > "${XOQ_SYSTEMD_DIR}/xoq-can@.service" <<UNIT
[Unit]
Description=XoQ CAN Server (%i)
PartOf=xoq.target
StartLimitIntervalSec=60
StartLimitBurst=10

[Service]
Type=simple
ExecStartPre=-/usr/bin/sudo /usr/sbin/ip link set %i down
ExecStartPre=/usr/bin/sudo /usr/sbin/ip link set %i up type can bitrate 1000000 dbitrate 5000000 fd on restart-ms 100
ExecStart=${BIN_DIR}/can-server %i:fd --key-dir ${XOQ_KEY_DIR} --moq-relay ${XOQ_RELAY} --moq-path anon/${XOQ_MACHINE_ID}/xoq-can-%i
Restart=always
RestartSec=5
Environment=RUST_LOG=xoq=info,warn

[Install]
WantedBy=xoq.target
UNIT
}

generate_fake_can_template() {
    cat > "${XOQ_SYSTEMD_DIR}/xoq-fake-can@.service" <<UNIT
[Unit]
Description=XoQ Fake CAN Server (%i)
PartOf=xoq.target
StartLimitIntervalSec=60
StartLimitBurst=10

[Service]
Type=simple
ExecStart=${BIN_DIR}/fake-can-server --key-dir ${XOQ_KEY_DIR} --moq-relay ${XOQ_RELAY} --moq-path anon/${XOQ_MACHINE_ID}/xoq-can-%i-test
Restart=always
RestartSec=5
Environment=RUST_LOG=xoq=info,warn

[Install]
WantedBy=xoq.target
UNIT
}

generate_realsense_template() {
    cat > "${XOQ_SYSTEMD_DIR}/xoq-realsense@.service" <<UNIT
[Unit]
Description=XoQ RealSense Server (%i)
PartOf=xoq.target
StartLimitIntervalSec=60
StartLimitBurst=10

[Service]
Type=simple
ExecStart=${BIN_DIR}/realsense-server --relay ${XOQ_RELAY} --path anon/${XOQ_MACHINE_ID}/realsense-%i --serial %i
Restart=always
RestartSec=5
Environment=RUST_LOG=xoq=info,warn

[Install]
WantedBy=xoq.target
UNIT
}

generate_camera_template() {
    cat > "${XOQ_SYSTEMD_DIR}/xoq-camera@.service" <<UNIT
[Unit]
Description=XoQ Camera Server (%i)
PartOf=xoq.target
StartLimitIntervalSec=60
StartLimitBurst=10

[Service]
Type=simple
ExecStart=${BIN_DIR}/camera-server %i --key-dir ${XOQ_KEY_DIR} --moq anon/${XOQ_MACHINE_ID}/camera-%i --relay ${XOQ_RELAY} --insecure
Restart=always
RestartSec=5
Environment=RUST_LOG=xoq=info,warn

[Install]
WantedBy=xoq.target
UNIT
}

generate_audio_service() {
    cat > "${XOQ_SYSTEMD_DIR}/xoq-audio.service" <<UNIT
[Unit]
Description=XoQ Audio Server
PartOf=xoq.target
StartLimitIntervalSec=60
StartLimitBurst=10

[Service]
Type=simple
ExecStart=${BIN_DIR}/audio-server --identity ${XOQ_KEY_DIR}/.xoq_audio_server_key --moq anon/${XOQ_MACHINE_ID}/audio
Restart=always
RestartSec=5
Environment=RUST_LOG=xoq=info,warn

[Install]
WantedBy=xoq.target
UNIT
}

# ============================================================================
# Generate launchd plists (macOS)
# ============================================================================
generate_launchd_plist() {
    local dest_dir="$1"
    local label="$2"
    local bin_path="$3"
    shift 3
    local args=("$@")

    local plist_path="${dest_dir}/${label}.plist"
    local log_dir="${XOQ_CONFIG_DIR}/logs"
    mkdir -p "$log_dir" "$dest_dir"

    local run_at_load="false"
    if [ "$ENABLE_BOOT" = true ]; then
        run_at_load="true"
    fi

    cat > "$plist_path" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>${label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>${bin_path}</string>
EOF

    for arg in "${args[@]}"; do
        echo "        <string>${arg}</string>" >> "$plist_path"
    done

    cat >> "$plist_path" <<EOF
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>RUST_LOG</key>
        <string>xoq=info,warn</string>
    </dict>
    <key>RunAtLoad</key>
    <${run_at_load}/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>${log_dir}/${label}.log</string>
    <key>StandardErrorPath</key>
    <string>${log_dir}/${label}.err</string>
</dict>
</plist>
EOF
}

# ============================================================================
# Check CAN sudo access (Linux only)
# ============================================================================
setup_can_sudo() {
    # Already have passwordless sudo?
    if sudo -n ip link show &>/dev/null 2>&1; then
        return 0
    fi

    # No passwordless sudo — set up the sudoers entry
    local sudoers_file="/etc/sudoers.d/xoq-can"
    info "Setting up passwordless sudo for CAN interfaces..."
    info "This requires your password once (creates ${sudoers_file})"
    echo "${USER} ALL=(root) NOPASSWD: /usr/sbin/ip link set can*" | sudo tee "$sudoers_file" >/dev/null && sudo chmod 0440 "$sudoers_file"

    # Verify it worked
    if sudo -n ip link show &>/dev/null 2>&1; then
        ok "CAN sudoers configured"
        return 0
    else
        warn "Failed to configure passwordless sudo for CAN"
        return 1
    fi
}

# ============================================================================
# Extract iroh NodeId from logs
# ============================================================================
extract_node_id() {
    local label="$1"
    local node_id=""
    local attempts=0

    # Retry up to 6 times (total ~15s) — services may take a moment to log their ID
    while [ $attempts -lt 6 ] && [ -z "$node_id" ]; do
        if [ "$PLATFORM" = "linux" ]; then
            node_id=$(journalctl --user -u "$label" --since "120s ago" --no-pager -o cat 2>/dev/null \
                | grep -oP '(?:Server ID|bridge server running\. ID): \K\S+' | tail -1 || true)
        else
            # macOS: check log file
            local log_file="${XOQ_CONFIG_DIR}/logs/${label}.log"
            if [ -f "$log_file" ]; then
                node_id=$(tail -100 "$log_file" 2>/dev/null | grep -oE '(Server ID|bridge server running\. ID): [^ ]+' | tail -1 | sed 's/.*ID: //' || true)
            fi
        fi
        if [ -z "$node_id" ]; then
            attempts=$((attempts + 1))
            [ $attempts -lt 6 ] && sleep 2
        fi
    done

    if [ -z "$node_id" ]; then
        echo "pending"
    else
        echo "$node_id"
    fi
}

# ============================================================================
# Generate machine.json
# ============================================================================
generate_machine_json() {
    local can_ifaces=($1)
    local rs_serials=($2)
    local cam_indices=($3)
    local has_audio="$4"

    local json='{'
    json+="\"machine_id\":\"${XOQ_MACHINE_ID}\","
    json+="\"hostname\":\"$(hostname)\","
    json+="\"platform\":\"${PLATFORM}\","
    json+="\"relay\":\"${XOQ_RELAY}\","
    json+="\"generated_at\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\","
    json+="\"services\":{"

    # CAN (real)
    json+="\"can\":["
    local first=true
    for iface in "${can_ifaces[@]+"${can_ifaces[@]}"}"; do
        [ -z "$iface" ] && continue
        local unit="xoq-can@${iface}.service"
        local node_id
        node_id=$(extract_node_id "$unit")
        $first || json+=","
        first=false
        json+="{\"interface\":\"${iface}\","
        json+="\"moq_path\":\"anon/${XOQ_MACHINE_ID}/xoq-can-${iface}\","
        json+="\"iroh_node_id\":\"${node_id}\","
        json+="\"unit\":\"${unit}\"}"
    done
    json+="],"

    # Fake CAN (simulated motors, mirrors discovered interfaces)
    local discovered_can=(${5:-})
    json+="\"fake_can\":["
    first=true
    for iface in "${discovered_can[@]+"${discovered_can[@]}"}"; do
        [ -z "$iface" ] && continue
        local unit="xoq-fake-can@${iface}.service"
        local node_id
        node_id=$(extract_node_id "$unit")
        $first || json+=","
        first=false
        json+="{\"interface\":\"${iface}\","
        json+="\"moq_path\":\"anon/${XOQ_MACHINE_ID}/xoq-can-${iface}-test\","
        json+="\"iroh_node_id\":\"${node_id}\","
        json+="\"unit\":\"${unit}\"}"
    done
    json+="],"

    # RealSense
    json+="\"realsense\":["
    first=true
    for serial in "${rs_serials[@]+"${rs_serials[@]}"}"; do
        [ -z "$serial" ] && continue
        local label
        if [ "$PLATFORM" = "linux" ]; then
            label="xoq-realsense@${serial}.service"
        else
            label="com.xoq.realsense-${serial}"
        fi
        $first || json+=","
        first=false
        json+="{\"serial\":\"${serial}\","
        json+="\"moq_path\":\"anon/${XOQ_MACHINE_ID}/realsense-${serial}\","
        json+="\"unit\":\"${label}\"}"
    done
    json+="],"

    # Cameras
    json+="\"cameras\":["
    first=true
    for idx in "${cam_indices[@]+"${cam_indices[@]}"}"; do
        [ -z "$idx" ] && continue
        local label
        if [ "$PLATFORM" = "linux" ]; then
            label="xoq-camera@${idx}.service"
        else
            label="com.xoq.camera-${idx}"
        fi
        $first || json+=","
        first=false
        json+="{\"index\":${idx},"
        json+="\"moq_path\":\"anon/${XOQ_MACHINE_ID}/camera-${idx}\","
        json+="\"unit\":\"${label}\"}"
    done
    json+="],"

    # Audio (MoQ only, no iroh NodeId)
    if [ "$has_audio" = "yes" ]; then
        local label
        if [ "$PLATFORM" = "linux" ]; then
            label="xoq-audio.service"
        else
            label="com.xoq.audio"
        fi
        json+="\"audio\":{\"moq_path\":\"anon/${XOQ_MACHINE_ID}/audio\","
        json+="\"unit\":\"${label}\"}"
    else
        json+="\"audio\":null"
    fi

    json+="}}"

    # Pretty-print if python3 is available, otherwise raw
    if command -v python3 &>/dev/null; then
        echo "$json" | python3 -m json.tool > "${XOQ_CONFIG_DIR}/machine.json"
    else
        echo "$json" > "${XOQ_CONFIG_DIR}/machine.json"
    fi
}

# ============================================================================
# Deploy — Linux (systemd)
# ============================================================================
deploy_linux() {
    local can_ifaces=("${CAN_IFACES[@]+"${CAN_IFACES[@]}"}")
    local rs_serials=("${RS_SERIALS[@]+"${RS_SERIALS[@]}"}")
    local cam_indices=("${CAM_INDICES[@]+"${CAM_INDICES[@]}"}")

    # --- CAN sudo setup ---
    if [ ${#can_ifaces[@]} -gt 0 ]; then
        header "CAN Setup"
        if setup_can_sudo; then
            ok "Passwordless sudo for ip link: available"
        else
            warn "Skipping real CAN services (no sudo access). Fake CAN will still be deployed."
            can_ifaces=()
            CAN_IFACES=()
        fi
    fi

    # --- Stop and clean up previous deployment ---
    header "Cleaning Previous Deployment"
    systemctl --user stop xoq.target 2>/dev/null || true
    for unit_file in "${XOQ_SYSTEMD_DIR}"/xoq-*.service; do
        [ -f "$unit_file" ] || continue
        local unit_name
        unit_name=$(basename "$unit_file")
        # Stop all running instances of template services
        if [[ "$unit_name" == *@.service ]]; then
            local prefix="${unit_name%%@.service}"
            for running in $(systemctl --user list-units --no-legend "${prefix}@*" 2>/dev/null | awk '{print $1}'); do
                systemctl --user stop "$running" 2>/dev/null || true
                systemctl --user disable "$running" 2>/dev/null || true
                systemctl --user reset-failed "$running" 2>/dev/null || true
            done
        else
            systemctl --user stop "$unit_name" 2>/dev/null || true
            systemctl --user disable "$unit_name" 2>/dev/null || true
        fi
        rm -f "$unit_file"
    done
    rm -f "${XOQ_SYSTEMD_DIR}/xoq.target"
    systemctl --user daemon-reload
    ok "Previous services stopped and removed"

    # --- Create directories ---
    mkdir -p "${XOQ_CONFIG_DIR}" "${XOQ_KEY_DIR}" "${XOQ_SYSTEMD_DIR}"

    # --- Generate env file + units ---
    header "Generating Configuration"
    generate_env_file
    ok "Environment file: ${XOQ_CONFIG_DIR}/env"

    generate_target
    ok "xoq.target"

    if [ ${#can_ifaces[@]} -gt 0 ]; then
        generate_can_template
        ok "xoq-can@.service"
    fi

    if [ ${#DISCOVERED_CAN[@]} -gt 0 ]; then
        generate_fake_can_template
        ok "xoq-fake-can@.service (simulated motors)"
    fi

    if [ ${#rs_serials[@]} -gt 0 ]; then
        generate_realsense_template
        ok "xoq-realsense@.service"
    fi

    if [ ${#cam_indices[@]} -gt 0 ]; then
        generate_camera_template
        ok "xoq-camera@.service"
    fi

    if [ "$HAS_AUDIO" = "yes" ]; then
        generate_audio_service
        ok "xoq-audio.service"
    fi

    # --- Reload systemd ---
    header "Starting Services"
    systemctl --user daemon-reload

    # With --boot: enable services + linger so they start on boot
    if [ "$ENABLE_BOOT" = true ]; then
        loginctl enable-linger "$(whoami)" 2>/dev/null || warn "loginctl enable-linger failed (may need root)"
        ok "User linger enabled (start-on-boot)"
        systemctl --user enable xoq.target
    fi

    for iface in "${can_ifaces[@]+"${can_ifaces[@]}"}"; do
        [ "$ENABLE_BOOT" = true ] && systemctl --user enable "xoq-can@${iface}.service"
        systemctl --user restart "xoq-can@${iface}.service"
        ok "Started xoq-can@${iface}.service"
    done

    for iface in "${DISCOVERED_CAN[@]+"${DISCOVERED_CAN[@]}"}"; do
        [ -z "$iface" ] && continue
        [ "$ENABLE_BOOT" = true ] && systemctl --user enable "xoq-fake-can@${iface}.service"
        systemctl --user restart "xoq-fake-can@${iface}.service"
        ok "Started xoq-fake-can@${iface}.service"
    done

    for serial in "${rs_serials[@]+"${rs_serials[@]}"}"; do
        [ "$ENABLE_BOOT" = true ] && systemctl --user enable "xoq-realsense@${serial}.service"
        systemctl --user restart "xoq-realsense@${serial}.service"
        ok "Started xoq-realsense@${serial}.service"
    done

    for idx in "${cam_indices[@]+"${cam_indices[@]}"}"; do
        [ "$ENABLE_BOOT" = true ] && systemctl --user enable "xoq-camera@${idx}.service"
        systemctl --user restart "xoq-camera@${idx}.service"
        ok "Started xoq-camera@${idx}.service"
    done

    if [ "$HAS_AUDIO" = "yes" ]; then
        [ "$ENABLE_BOOT" = true ] && systemctl --user enable xoq-audio.service
        systemctl --user restart xoq-audio.service
        ok "Started xoq-audio.service"
    fi

    systemctl --user start xoq.target

    # --- Final status ---
    header "Service Status"
    systemctl --user list-units 'xoq-*' --no-legend 2>/dev/null || true
}

# ============================================================================
# Deploy — macOS (launchd)
# ============================================================================
deploy_macos() {
    local rs_serials=("${RS_SERIALS[@]+"${RS_SERIALS[@]}"}")
    local cam_indices=("${CAM_INDICES[@]+"${CAM_INDICES[@]}"}")

    # --- Stop and clean up previous deployment ---
    header "Cleaning Previous Deployment"
    for dir in "${XOQ_AGENTS_DIR}" "${XOQ_LAUNCHD_DIR}"; do
        for plist in "${dir}"/com.xoq.*.plist; do
            [ -f "$plist" ] || continue
            local label
            label=$(basename "$plist" .plist)
            launchctl bootout "gui/$(id -u)/${label}" 2>/dev/null || true
            rm -f "$plist"
        done
    done
    ok "Previous services stopped and removed"

    # With --boot: plists go to ~/Library/LaunchAgents (auto-loads on login)
    # Without:     plists go to ~/.config/xoq/agents (manual load only)
    local plist_dir="${XOQ_AGENTS_DIR}"
    if [ "$ENABLE_BOOT" = true ]; then
        plist_dir="${XOQ_LAUNCHD_DIR}"
    fi

    # --- Create directories ---
    mkdir -p "${XOQ_CONFIG_DIR}" "${XOQ_KEY_DIR}" "$plist_dir" "${XOQ_CONFIG_DIR}/logs"

    # --- Generate env file ---
    header "Generating Configuration"
    generate_env_file
    ok "Environment file: ${XOQ_CONFIG_DIR}/env"
    if [ "$ENABLE_BOOT" = true ]; then
        ok "Boot: enabled (plists in ~/Library/LaunchAgents)"
    else
        info "Boot: disabled (use --boot to start on login)"
    fi

    # --- Generate launchd plists ---

    for serial in "${rs_serials[@]+"${rs_serials[@]}"}"; do
        local label="com.xoq.realsense-${serial}"
        generate_launchd_plist "$plist_dir" "$label" \
            "${BIN_DIR}/realsense-server" \
            --relay "${XOQ_RELAY}" \
            --path "anon/${XOQ_MACHINE_ID}/realsense-${serial}" \
            --serial "$serial"
        ok "${label}"
    done

    for idx in "${cam_indices[@]+"${cam_indices[@]}"}"; do
        local label="com.xoq.camera-${idx}"
        generate_launchd_plist "$plist_dir" "$label" \
            "${BIN_DIR}/camera-server" \
            "$idx" \
            --key-dir "${XOQ_KEY_DIR}" \
            --moq "anon/${XOQ_MACHINE_ID}/camera-${idx}" \
            --relay "${XOQ_RELAY}" \
            --insecure
        ok "${label}"
    done

    if [ "$HAS_AUDIO" = "yes" ]; then
        local label="com.xoq.audio"
        generate_launchd_plist "$plist_dir" "$label" \
            "${BIN_DIR}/audio-server" \
            --identity "${XOQ_KEY_DIR}/.xoq_audio_server_key" \
            --moq "anon/${XOQ_MACHINE_ID}/audio"
        ok "${label}"
    fi

    # --- Load plists ---
    header "Starting Services"
    for plist in "${plist_dir}"/com.xoq.*.plist; do
        [ -f "$plist" ] || continue
        local label
        label=$(basename "$plist" .plist)
        # Unload first if already loaded
        launchctl bootout "gui/$(id -u)/${label}" 2>/dev/null || true
        launchctl bootstrap "gui/$(id -u)" "$plist"
        ok "Started ${label}"
    done

    # --- Status ---
    header "Service Status"
    launchctl list 2>/dev/null | grep "com.xoq" || info "No services running yet"
}

# ============================================================================
# Deploy mode (main)
# ============================================================================
do_deploy() {
    header "XoQ Deploy"
    echo "Machine ID: ${XOQ_MACHINE_ID}"
    echo "Platform:   ${PLATFORM}"
    echo "Relay:      ${XOQ_RELAY}"
    echo "Key dir:    ${XOQ_KEY_DIR}"

    # --- Stop existing services so hardware is not locked during discovery ---
    if [ "$PLATFORM" = "linux" ]; then
        systemctl --user stop xoq.target 2>/dev/null || true
        for running in $(systemctl --user list-units --no-legend 'xoq-*' 2>/dev/null | awk '{print $1}'); do
            systemctl --user stop "$running" 2>/dev/null || true
        done
    elif [ "$PLATFORM" = "macos" ]; then
        for dir in "${XOQ_AGENTS_DIR}" "${XOQ_LAUNCHD_DIR}"; do
            for plist in "${dir}"/com.xoq.*.plist; do
                [ -f "$plist" ] || continue
                local label
                label=$(basename "$plist" .plist)
                launchctl bootout "gui/$(id -u)/${label}" 2>/dev/null || true
            done
        done
    fi

    # --- Discover hardware ---
    header "Hardware Discovery"

    read -ra CAN_IFACES <<< "$(discover_can)"
    read -ra RS_SERIALS <<< "$(discover_realsense)"
    read -ra CAM_INDICES <<< "$(discover_cameras)"
    HAS_AUDIO=$(discover_audio)

    if [ ${#CAN_IFACES[@]} -gt 0 ] && [ -n "${CAN_IFACES[0]:-}" ]; then
        ok "CAN interfaces: ${CAN_IFACES[*]}"
    else
        info "No CAN interfaces found"
        CAN_IFACES=()
    fi
    # Save discovered interfaces for fake-can (deployed regardless of sudo)
    DISCOVERED_CAN=("${CAN_IFACES[@]+"${CAN_IFACES[@]}"}")

    if [ ${#RS_SERIALS[@]} -gt 0 ] && [ -n "${RS_SERIALS[0]:-}" ]; then
        ok "RealSense cameras: ${RS_SERIALS[*]}"
    else
        info "No RealSense cameras found"
        RS_SERIALS=()
    fi

    if [ ${#CAM_INDICES[@]} -gt 0 ] && [ -n "${CAM_INDICES[0]:-}" ]; then
        ok "Cameras: indices ${CAM_INDICES[*]}"
    else
        info "No cameras found"
        CAM_INDICES=()
    fi

    if [ "$HAS_AUDIO" = "yes" ]; then
        ok "Audio input devices found"
    else
        info "No audio input devices found"
    fi

    # Check if we found anything
    if [ ${#CAN_IFACES[@]} -eq 0 ] && [ ${#RS_SERIALS[@]} -eq 0 ] && \
       [ ${#CAM_INDICES[@]} -eq 0 ] && [ "$HAS_AUDIO" != "yes" ]; then
        warn "No hardware discovered. Nothing to deploy."
        exit 0
    fi

    # --- Build ---
    if [ "$DO_BUILD" = true ]; then
        do_build "${CAN_IFACES[*]}" "${RS_SERIALS[*]}" "${CAM_INDICES[*]}" "$HAS_AUDIO"
    fi

    if [ "$DRY_RUN" = true ]; then
        # --- Plan summary ---
        header "Planned Services"
        for iface in "${CAN_IFACES[@]+"${CAN_IFACES[@]}"}"; do
            echo "  xoq-can@${iface}.service → anon/${XOQ_MACHINE_ID}/xoq-can-${iface}"
        done
        for iface in "${DISCOVERED_CAN[@]+"${DISCOVERED_CAN[@]}"}"; do
            [ -z "$iface" ] && continue
            echo "  xoq-fake-can@${iface}.service → anon/${XOQ_MACHINE_ID}/xoq-can-${iface}-test"
        done
        for serial in "${RS_SERIALS[@]+"${RS_SERIALS[@]}"}"; do
            if [ "$PLATFORM" = "linux" ]; then
                echo "  xoq-realsense@${serial}.service → anon/${XOQ_MACHINE_ID}/realsense-${serial}"
            else
                echo "  com.xoq.realsense-${serial} → anon/${XOQ_MACHINE_ID}/realsense-${serial}"
            fi
        done
        for idx in "${CAM_INDICES[@]+"${CAM_INDICES[@]}"}"; do
            if [ "$PLATFORM" = "linux" ]; then
                echo "  xoq-camera@${idx}.service → anon/${XOQ_MACHINE_ID}/camera-${idx}"
            else
                echo "  com.xoq.camera-${idx} → anon/${XOQ_MACHINE_ID}/camera-${idx}"
            fi
        done
        if [ "$HAS_AUDIO" = "yes" ]; then
            if [ "$PLATFORM" = "linux" ]; then
                echo "  xoq-audio.service → anon/${XOQ_MACHINE_ID}/audio"
            else
                echo "  com.xoq.audio → anon/${XOQ_MACHINE_ID}/audio"
            fi
        fi
        echo ""
        info "Dry run complete. No changes made."
        exit 0
    fi

    # --- Validate binaries ---
    header "Binary Validation"
    local missing=false

    if [ ${#CAN_IFACES[@]} -gt 0 ]; then
        local bin; bin=$(find_bin can-server)
        if [ -n "$bin" ]; then ok "can-server: ${bin}"; else err "can-server not found"; missing=true; fi
    fi
    if [ ${#DISCOVERED_CAN[@]} -gt 0 ] && [ -n "${DISCOVERED_CAN[0]:-}" ]; then
        local bin; bin=$(find_bin fake-can-server)
        if [ -n "$bin" ]; then ok "fake-can-server: ${bin}"; else err "fake-can-server not found"; missing=true; fi
    fi
    if [ ${#RS_SERIALS[@]} -gt 0 ]; then
        local bin; bin=$(find_bin realsense-server)
        if [ -n "$bin" ]; then ok "realsense-server: ${bin}"; else err "realsense-server not found"; missing=true; fi
    fi
    if [ ${#CAM_INDICES[@]} -gt 0 ]; then
        local bin; bin=$(find_bin camera-server)
        if [ -n "$bin" ]; then ok "camera-server: ${bin}"; else err "camera-server not found"; missing=true; fi
    fi
    if [ "$HAS_AUDIO" = "yes" ]; then
        local bin; bin=$(find_bin audio-server)
        if [ -n "$bin" ]; then ok "audio-server: ${bin}"; else err "audio-server not found"; missing=true; fi
    fi

    if [ "$missing" = true ]; then
        err "Missing binaries. Build with: cargo build --release"
        exit 1
    fi

    # --- Platform-specific deploy ---
    if [ "$PLATFORM" = "linux" ]; then
        deploy_linux
    else
        deploy_macos
    fi

    # --- Wait for services and extract NodeIds ---
    header "Extracting Iroh NodeIds"
    info "Waiting 5 seconds for services to initialize..."
    sleep 5

    generate_machine_json \
        "${CAN_IFACES[*]}" \
        "${RS_SERIALS[*]}" \
        "${CAM_INDICES[*]}" \
        "$HAS_AUDIO" \
        "${DISCOVERED_CAN[*]}"

    ok "machine.json written to ${XOQ_CONFIG_DIR}/machine.json"
    echo ""
    cat "${XOQ_CONFIG_DIR}/machine.json"
    echo ""
    ok "Deploy complete!"
}

# ============================================================================
# JSON-only mode
# ============================================================================
do_json() {
    header "Regenerating machine.json"

    read -ra CAN_IFACES <<< "$(discover_can)"
    read -ra RS_SERIALS <<< "$(discover_realsense)"
    read -ra CAM_INDICES <<< "$(discover_cameras)"
    HAS_AUDIO=$(discover_audio)

    # Reset empty arrays
    [ ${#CAN_IFACES[@]} -gt 0 ] && [ -z "${CAN_IFACES[0]:-}" ] && CAN_IFACES=()
    [ ${#RS_SERIALS[@]} -gt 0 ] && [ -z "${RS_SERIALS[0]:-}" ] && RS_SERIALS=()
    [ ${#CAM_INDICES[@]} -gt 0 ] && [ -z "${CAM_INDICES[0]:-}" ] && CAM_INDICES=()

    # Fallback: if hardware discovery missed devices (e.g. busy), check running services
    if [ "$PLATFORM" = "linux" ] && [ ${#RS_SERIALS[@]} -eq 0 ]; then
        while IFS= read -r unit; do
            local serial="${unit#xoq-realsense@}"
            serial="${serial%.service}"
            [ -n "$serial" ] && RS_SERIALS+=("$serial")
        done < <(systemctl --user list-units --no-legend 'xoq-realsense@*' 2>/dev/null | awk '{print $1}')
        if [ ${#RS_SERIALS[@]} -gt 0 ]; then
            info "RealSense discovered from running services: ${RS_SERIALS[*]}"
        fi
    fi

    # For --json mode, DISCOVERED_CAN is the same as CAN_IFACES (no sudo filtering)
    DISCOVERED_CAN=("${CAN_IFACES[@]+"${CAN_IFACES[@]}"}")

    mkdir -p "${XOQ_CONFIG_DIR}"

    generate_machine_json \
        "${CAN_IFACES[*]}" \
        "${RS_SERIALS[*]}" \
        "${CAM_INDICES[*]}" \
        "$HAS_AUDIO" \
        "${DISCOVERED_CAN[*]}"

    ok "machine.json written to ${XOQ_CONFIG_DIR}/machine.json"
    cat "${XOQ_CONFIG_DIR}/machine.json"
}

# ============================================================================
# Dispatch
# ============================================================================
case "$MODE" in
    status)    do_status ;;
    uninstall)
        if [ "$DRY_RUN" = true ]; then
            info "Dry run: would uninstall all xoq services"
            exit 0
        fi
        do_uninstall
        ;;
    json)      do_json ;;
    deploy)    do_deploy ;;
esac

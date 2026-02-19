#!/usr/bin/env bash
# setup-can.sh — Detect and configure all available CAN interfaces.
#
# Brings interfaces down, then back up with CAN FD (1 Mbps nominal, 5 Mbps data)
# and auto-restart from BUS-OFF (restart-ms 100).
#
# Usage:
#   bash setup-can.sh           # Configure all detected CAN interfaces
#   bash setup-can.sh can0 can1 # Configure only specific interfaces

set -euo pipefail

BITRATE=1000000
DBITRATE=5000000
RESTART_MS=100

# ============================================================================
# Detect or use provided interfaces
# ============================================================================
if [ $# -gt 0 ]; then
    INTERFACES=("$@")
else
    # Auto-detect CAN interfaces from /sys/class/net
    INTERFACES=()
    for iface in /sys/class/net/can*; do
        [ -e "$iface" ] || continue
        INTERFACES+=("$(basename "$iface")")
    done

    if [ ${#INTERFACES[@]} -eq 0 ]; then
        echo "No CAN interfaces found."
        echo "Check that your CAN adapter (e.g. PCAN USB Pro FD) is plugged in."
        exit 1
    fi
fi

echo "CAN interfaces: ${INTERFACES[*]}"
echo "Settings: bitrate=${BITRATE}, dbitrate=${DBITRATE}, fd=on, restart-ms=${RESTART_MS}"
echo ""

# ============================================================================
# Stop can-server if running (it holds stale socket handles)
# ============================================================================
if systemctl --user is-active can-server &>/dev/null; then
    echo "Stopping can-server (stale sockets cause 'No such device' errors)..."
    systemctl --user stop can-server
    RESTART_SERVER=true
else
    RESTART_SERVER=false
fi

# ============================================================================
# Bring interfaces down then up
# ============================================================================
for iface in "${INTERFACES[@]}"; do
    echo "  [$iface] down..."
    sudo ip link set "$iface" down 2>/dev/null || true

    echo "  [$iface] up (CAN FD, ${BITRATE}/${DBITRATE}, restart-ms ${RESTART_MS})"
    sudo ip link set "$iface" up type can \
        bitrate "$BITRATE" \
        dbitrate "$DBITRATE" \
        fd on \
        restart-ms "$RESTART_MS"
done

echo ""

# ============================================================================
# Restart can-server if it was running
# ============================================================================
if [ "$RESTART_SERVER" = true ]; then
    echo "Restarting can-server..."
    systemctl --user start can-server
fi

# ============================================================================
# Verify
# ============================================================================
echo ""
echo "Interface status:"
for iface in "${INTERFACES[@]}"; do
    state=$(cat "/sys/class/net/$iface/operstate" 2>/dev/null || echo "unknown")
    echo "  $iface: $state"
done

#!/usr/bin/env bash
# fetch-machines.sh — Fetch machine.json from all robots and merge into a local fleet config.
#
# Output: ~/.config/xoq/machines.json (array of machine configs)
#
# Usage:
#   scripts/fetch-machines.sh                           # Default: baguette + champagne
#   scripts/fetch-machines.sh user1@host1 user2@host2   # Custom hosts

set -euo pipefail

XOQ_CONFIG_DIR="${HOME}/.config/xoq"
OUTPUT="${XOQ_CONFIG_DIR}/machines.json"
REMOTE_PATH=".config/xoq/machine.json"

# Default hosts
DEFAULT_HOSTS=(
    "baguette@172.18.128.205"
    "champagne@172.18.128.207"
)

HOSTS=("${@:-${DEFAULT_HOSTS[@]}}")

mkdir -p "$XOQ_CONFIG_DIR"

machines="["
first=true

for host in "${HOSTS[@]}"; do
    echo -n "  ${host} ... "
    json=$(ssh -o ConnectTimeout=5 "$host" "cat ~/${REMOTE_PATH}" 2>/dev/null) || {
        echo "FAILED (unreachable or no machine.json)"
        continue
    }
    $first || machines+=","
    first=false
    machines+="$json"
    echo "ok"
done

machines+="]"

if command -v python3 &>/dev/null; then
    echo "$machines" | python3 -m json.tool > "$OUTPUT"
else
    echo "$machines" > "$OUTPUT"
fi

echo ""
echo "Written to ${OUTPUT}"
echo ""
cat "$OUTPUT"

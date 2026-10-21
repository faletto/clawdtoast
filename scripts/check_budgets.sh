#!/bin/bash
# Resource budgets from the design doc. Usage: scripts/check_budgets.sh [path/to/release/clawdtoast]
# Set CLAWDTOAST_CI_GUI=1 on a machine with a window server to also check the overlay, and
# CLAWDTOAST_CI_GUI_ONLY=1 to check only the overlay.
set -u
BIN="${1:-target/release/clawdtoast}"
BIN="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"

MAX_SIZE_KB=3072        # release binary
MAX_IDLE_RSS_KB=12288   # daemon, no card showing (the UI lives in a separate short-lived process)
MAX_OVERLAY_RSS_KB=92160 # overlay process while 3 cards show
MAX_SHIM_P95_MS=20
MAX_LATENCY_MS=250      # event -> first card drawn, cold daemon

export CLAWDTOAST_DIR="$(mktemp -d)" CLAWDTOAST_MUTE=1 CLAWDTOAST_PID=$$
fail=0
report() { # name value limit unit
  if [ "$2" -le "$3" ]; then echo "ok   $1: $2 $4 (limit $3)"; else echo "FAIL $1: $2 $4 (limit $3)"; fail=1; fi
}
cleanup() { pkill -f "$BIN daemon" 2>/dev/null; pkill -f "$BIN overlay" 2>/dev/null; rm -rf "$CLAWDTOAST_DIR"; }
trap cleanup EXIT

report "binary size" "$(( $(stat -c %s "$BIN" 2>/dev/null || stat -f %z "$BIN") / 1024 ))" $MAX_SIZE_KB KB

if [ -z "${CLAWDTOAST_CI_GUI_ONLY:-}" ]; then
# Idle daemon memory, 10 s after start, with one registered session.
CLAWDTOAST_GRACE_SECS=600 CLAWDTOAST_HEADLESS=1 "$BIN" hook session-start <<< '{"session_id":"bud","cwd":"/x/proj"}'
sleep 10
DPID=$(pgrep -n -f "$BIN daemon")
report "daemon idle rss" "$(ps -o rss= -p "$DPID" | tr -d ' ')" $MAX_IDLE_RSS_KB KB

# Shim run time with the daemon already running (p95 over 40 runs).
P95=$(python3 -I - "$BIN" <<'EOF'
import subprocess, sys, time, json
inp = json.dumps({"session_id": "bud", "cwd": "/x/proj", "notification_type": "idle_prompt", "message": "hi"}).encode()
ts = []
for _ in range(40):
    t = time.perf_counter()
    subprocess.run([sys.argv[1], "hook", "notification"], input=inp)
    ts.append((time.perf_counter() - t) * 1000)
    time.sleep(0.02)
ts.sort()
print(int(ts[37]) + 1)
EOF
)
report "shim p95" "$P95" $MAX_SHIM_P95_MS ms
fi

pkill -f "$BIN daemon"; sleep 0.5; rm -rf "$CLAWDTOAST_DIR"; mkdir -p "$CLAWDTOAST_DIR"

if [ "${CLAWDTOAST_CI_GUI:-}" = "1" ]; then
  export CLAWDTOAST_DURATION_SECS=8
  for i in 1 2 3; do
    echo "{\"session_id\":\"g$i\",\"cwd\":\"/x/p$i\",\"notification_type\":\"idle_prompt\",\"message\":\"card $i\"}" | "$BIN" hook notification
  done
  sleep 2
  LAT=$(grep -o 'latency_ms=[0-9]*' "$CLAWDTOAST_DIR/clawdtoast.log" | head -1 | cut -d= -f2)
  report "cold start -> first card" "${LAT:-99999}" $MAX_LATENCY_MS ms
  OPID=$(pgrep -n -f "$BIN overlay")
  report "overlay rss, 3 cards" "$(ps -o rss= -p "${OPID:-0}" | tr -d ' ' | sed 's/^$/99999999/')" $MAX_OVERLAY_RSS_KB KB
else
  echo "skip overlay checks (set CLAWDTOAST_CI_GUI=1 on a machine with a window server)"
fi
exit $fail

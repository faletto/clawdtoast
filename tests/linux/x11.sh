#!/bin/bash
# X11 overlay scenario, run inside the container (see run.sh). Starts Xvfb (optionally with a
# compositor), shows cards, and checks the windows through xwininfo; screenshots go to $OUT.
# Usage: x11.sh BIN OUT [compositor|wayland-fallback]
# wayland-fallback sets WAYLAND_DISPLAY to a compositor that cannot be reached, like a GNOME
# session whose Mutter has no layer-shell: the cards must still appear, through the X server.
set -u
BIN=$1 OUT=$2 COMP=${3:-}
mkdir -p "$OUT"
export CLAWDTOAST_DIR="$(mktemp -d)" CLAWDTOAST_MUTE=1 CLAWDTOAST_PID=$$ CLAWDTOAST_GRACE_SECS=1
if [ "$COMP" = wayland-fallback ]; then export WAYLAND_DISPLAY=wayland-does-not-exist; else unset WAYLAND_DISPLAY; fi
# On a shared machine (CI) a daemon left by an earlier scenario would skew the process checks.
pkill -f "$BIN (daemon|overlay)" 2>/dev/null; sleep 0.5
trap 'pkill -f "$BIN (daemon|overlay)" 2>/dev/null' EXIT
export DISPLAY=:99
fail=0
check() { if eval "$2"; then echo "ok   $1"; else echo "FAIL $1"; fail=1; fi; }

Xvfb :99 -screen 0 1280x720x24 +extension RANDR >/dev/null 2>&1 &
XPID=$!
for _ in $(seq 50); do xdpyinfo >/dev/null 2>&1 && break; sleep 0.1; done
if [ "$COMP" = compositor ]; then xcompmgr >/dev/null 2>&1 & sleep 0.5; fi
tag=${COMP:-plain}

echo '{"session_id":"aaaa1111","cwd":"/x/proj"}' | "$BIN" hook session-start
"$BIN" test &   # three cards, 1.2 s apart
TESTPID=$!
sleep 3.2

TREE=$(xwininfo -root -tree)
check "overlay window exists" "echo \"\$TREE\" | grep -q 'clawdtoast'"
n=$(echo "$TREE" | grep -c '"clawdtoast"')
check "three cards on screen" "[ $n -eq 3 ]"
id=$(echo "$TREE" | grep '"clawdtoast"' | head -1 | awk '{print $1}')
if [ -z "$id" ]; then # xwininfo -id with no id waits for a mouse click, so bail out
  echo "no card window found; log:"; cat "$CLAWDTOAST_DIR/clawdtoast.log"
  echo "$TREE" | head -20
  kill $XPID 2>/dev/null
  exit 1
fi
info=$(xwininfo -id "$id")
check "card is 320x96" "echo \"\$info\" | grep -q 'Width: 320' && echo \"\$info\" | grep -q 'Height: 96'"
check "card is override-redirect" "xwininfo -id $id -all | grep -q 'Override Redirect State: yes'"
# Top-right corner: x + 320 + 16 margin == 1280.
x=$(echo "$info" | awk '/Absolute upper-left X/ {print $4}')
check "placed at the right edge with a 16px margin" "[ $x -eq $((1280 - 320 - 16)) ]"
check "never takes the input focus" "! xdotool getwindowfocus 2>/dev/null | grep -qx \"\$(printf '%d' $id)\""
import -window root "$OUT/x11-$tag.png"
echo "info overlay rss with 3 cards: $(ps -o rss= -p "$(pgrep -n -f "$BIN overlay")" | tr -d ' ') KB"

wait $TESTPID   # not a bare `wait`: that would also wait for Xvfb
sleep 6.5
n=$(xwininfo -root -tree | grep -c '"clawdtoast"')
check "cards gone after timeout" "[ $n -eq 0 ]"
echo '{"session_id":"aaaa1111"}' | "$BIN" hook session-end
sleep 4
check "overlay and daemon exit when idle" "! pgrep -f 'clawdtoast (overlay|daemon)' >/dev/null"
grep -E "latency_ms|x11|overlay" "$CLAWDTOAST_DIR/clawdtoast.log" | tail -8

kill $XPID 2>/dev/null
exit $fail

#!/bin/bash
# Builds clawdtoast for Linux in a container and runs display scenarios against it.
# Usage: tests/linux/run.sh [test|x11|x11-comp|x11-wayland|wayland|fallback|lifecycle|all]  (default: all)
# Screenshots land in $OUT (default: ./target/linux-shots). Needs Docker (colima works).
set -eu
cd "$(dirname "$0")/../.."
ROOT=$PWD
OUT=${OUT:-$ROOT/target/linux-shots}
mkdir -p "$OUT"
IMAGE=clawdtoast-linux
docker image inspect "$IMAGE" >/dev/null 2>&1 || docker build -t "$IMAGE" tests/linux

inside() { # run a command in the container with the source mounted
  docker run --rm \
    -v "$ROOT":/src -v clawdtoast-cargo:/root/.cargo/registry -v clawdtoast-target:/cargo-target \
    -v "$OUT":/out -e CARGO_TARGET_DIR=/cargo-target -e RUST_BACKTRACE=1 \
    "$IMAGE" "$@"
}

BIN=/cargo-target/release/clawdtoast
scenario=${1:-all}
case $scenario in
  test)      inside cargo test ;;
  x11)       inside tests/linux/build-and-run.sh tests/linux/x11.sh $BIN /out ;;
  x11-comp)  inside tests/linux/build-and-run.sh tests/linux/x11.sh $BIN /out compositor ;;
  x11-wayland) inside tests/linux/build-and-run.sh tests/linux/x11.sh $BIN /out wayland-fallback ;;
  wayland)   inside tests/linux/build-and-run.sh tests/linux/wayland.sh $BIN /out ;;
  fallback)  inside tests/linux/build-and-run.sh tests/linux/fallback.sh $BIN ;;
  lifecycle) inside tests/linux/build-and-run.sh tests/integration.sh $BIN ;;
  budgets)   inside tests/linux/build-and-run.sh scripts/check_budgets.sh $BIN ;;
  all)
    for s in test lifecycle budgets x11 x11-comp x11-wayland wayland fallback; do
      echo "=== $s"
      "$0" "$s"
    done ;;
  *) echo "unknown scenario $scenario"; exit 2 ;;
esac

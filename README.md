# clawdtoast

A tiny Clawd waves in the corner of your screen, with a chime, when Claude Code finishes a turn or needs you. It runs only while a Claude Code session is open. Works on macOS and on Linux (Wayland or X11); see the design doc for the plan.

## Install

### As a Claude Code plugin (recommended)

```
/plugin marketplace add /path/to/clawdtoast      # or the git URL once pushed
/plugin install clawdtoast@clawdtoast
```

The plugin's hooks call `scripts/clawdtoast.sh`, which runs the first binary it finds: `$CLAWDTOAST_BIN`, `<plugin>/bin/clawdtoast`, `<plugin>/target/release/clawdtoast`, then `clawdtoast` on `PATH`. With none of those, the first `SessionStart` builds one in the background with `cargo build --release` (about 20 s); hooks do nothing until it finishes.

### Without the plugin

```
cargo build --release
target/release/clawdtoast install      # adds the hooks to ~/.claude/settings.json (backup saved first)
target/release/clawdtoast uninstall    # removes only clawdtoast's entries
```

`install` refuses to run while the plugin is installed (the hooks would fire twice) unless you pass `--force`. Try it any time with `clawdtoast test`, which shows one card of each kind.

### Linux

Same plugin or `install` steps; `cargo build --release` needs a Rust toolchain and the ALSA headers (`libasound2-dev` on Debian/Ubuntu, `alsa-lib-dev` on Alpine, `alsa-lib` on Arch/Fedora).

The overlay picks a backend at runtime:

| Session | Backend | Notes |
|---|---|---|
| KDE Plasma, sway, Hyprland, other compositors with `wlr-layer-shell` | Wayland layer-shell | Cards sit on the overlay layer in the corner of the active output, above full-screen windows, and never take focus. Fractional scaling is honoured. |
| GNOME (Mutter has no layer-shell), plain X11 | X11 override-redirect windows (XWayland under GNOME) | Needs `DISPLAY`, which a terminal in the session already has. Cards sit below the top bar. A native Wayland full-screen app can cover them. |

Linux differences: clicking a card dismisses it but cannot bring the terminal forward (neither Wayland nor X11 lets one app raise another's window); fonts come from Noto Sans, DejaVu Sans or Liberation Sans if installed; state lives in `~/.local/state/clawdtoast` unless `CLAWDTOAST_DIR` is set. Sound needs a working ALSA/PipeWire/PulseAudio output; with none, the card still shows and the log says `sound: no output device`.

## What it does

| Claude Code event | Card | Sound |
|---|---|---|
| `Stop` | Done · project, first sentence of the last assistant message | chime |
| `Notification` `permission_prompt` | Needs permission · project, the hook's message | ping |
| `Notification` `idle_prompt` | Waiting for you · project, the hook's message | ping |
| `SubagentStop` | off by default | ping |

- Cards sit in a corner of the screen holding the mouse (top-right by default; on Linux, the active output or the monitor under the pointer), above full-screen apps and on every Space, and never take focus, neither when they appear nor when you click one (no Accessibility permission needed on macOS). Clawd waves at 8 fps.
- Up to 3 cards stack; more wait in a queue. A new alert for a session that already has a card replaces it. With more than one session open, the title carries a 4-character session tag.
- A card fades in over 150 ms, stays 5 s, fades out over 300 ms. Hovering pauses the timer; clicking dismisses it.
- The daemon starts on the first `SessionStart`, forgets sessions whose Claude Code process died (checked every 30 s), and exits 10 s after the last session closes. It draws nothing itself: it starts a short-lived `clawdtoast overlay` process when a card is due, which exits 3 s after the last card disappears. That keeps the always-running part small.

## Config

`~/.config/clawdtoast/config.toml`, every key optional. Run `clawdtoast reload` to apply changes to a running daemon (a card already on screen keeps the old settings).

```toml
duration_secs = 5
position = "top-right"      # top-left | top-right | bottom-left | bottom-right
max_stack = 3               # 1-6

[sound]
enabled = true
volume = 0.6
# done = "~/sounds/ding.wav"       # WAV only
# attention = "~/sounds/ping.wav"

[events]
stop = true
permission_prompt = true
idle_prompt = true
subagent_stop = false
```

Environment overrides (handy for tests): `CLAWDTOAST_CONFIG`, `CLAWDTOAST_DURATION_SECS`, `CLAWDTOAST_MUTE`, `CLAWDTOAST_SUBAGENT`, `CLAWDTOAST_DIR` (socket/lock/log dir, default `~/Library/Application Support/clawdtoast` on macOS, `~/.local/state/clawdtoast` on Linux), `CLAWDTOAST_GRACE_SECS` (10), `CLAWDTOAST_LIVENESS_SECS` (30), `CLAWDTOAST_HEADLESS` (log cards instead of drawing them). Log: `clawdtoast.log` in the support dir.

## Development

```
cargo test                          # unit tests
tests/integration.sh [binary]       # headless lifecycle, config, install/uninstall tests
scripts/check_budgets.sh [binary]   # resource budgets (CLAWDTOAST_CI_GUI=1 adds the overlay checks)
python3 -I tools/gen_assets.py      # regenerate assets/ (sprite sheet + sounds)
scripts/release.sh                  # universal binary in dist/, signed (see below)
```

CI (`.github/workflows/ci.yml`) runs the first three on macOS and on Linux. The Linux job also runs the overlay checks on X11 (Xvfb) and on Wayland (headless sway), using `CLAWDTOAST_CI_GUI=1` (and `CLAWDTOAST_CI_GUI_ONLY=1` to skip the daemon and shim checks).

### Budgets

| Metric | Limit | Measured here |
|---|---|---|
| Release binary | 3 MB | 1.1 MB (macOS), 1.4 MB (Linux) |
| Daemon memory, idle | 12 MB (design doc said 8; see below) | 8.9 MB |
| Overlay memory, 3 cards showing | 90 MB | 69 MB |
| Hook shim p95 | 20 ms | 16 ms |
| Event to first card, cold daemon | 250 ms | 135 ms |

The design doc's 8 MB / 20 MB assumed one process doing everything; AppKit alone costs about 30 MB, so the UI moved to the short-lived overlay process and the budgets were re-cut around that split.

### Signing and notarization

`scripts/release.sh` builds arm64 + x86_64, joins them with `lipo`, signs, and zips. With `CODESIGN_IDENTITY="Developer ID Application: ..."` it signs for distribution, and with `NOTARY_PROFILE` it submits to Apple's notary service. **Only the ad-hoc path has been run**: no Developer ID certificate was available when this was written, so signing with one and notarizing are untested.

## Known gaps

- **Font:** loads Arial (macOS) or Noto/DejaVu/Liberation Sans (Linux) from the system at runtime; embedding an open-licensed subset is still open.
- **Do Not Disturb / Focus muting** (design doc config key `mute_in_focus_mode`) is not implemented; macOS exposes no public API for it.
- **Artwork:** `assets/clawd.png` is a hand-drawn placeholder sheet (4 frames side by side, frame size = image height). Replace it with the official art and nothing else changes. Check licensing before making the repo public.
- **Overlay race:** an alert that arrives in the instant the idle overlay exits is dropped (the daemon retries only when the pipe is already closed).
- `vendor/softbuffer` is softbuffer 0.4.8 with a one-line patch so macOS honours alpha (see `vendor/softbuffer/PATCH.md`). `fontdue` is pinned to 0.9.3 because 0.9.5 needs a newer Rust than 1.86.
- **Linux:** no click-to-focus of the hosting terminal; the X11 path reads the UI scale from `Xft.dpi` and has only been checked under GNOME's automatic scaling; sound has only been verified through a virtual audio device. Overlay memory is about 25 MB for 3 cards.

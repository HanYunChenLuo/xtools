# XTools

A collection of development tools for Android development.

## Projects

| Tool | Description |
|------|-------------|
| `xperf-cli` | CLI: Android app performance monitor (CPU / memory / FPS / GPU / trace / capture / ...) |
| `xperf-gui` | Tauri 2 GUI: real-time charts for the same metrics |
| `xperf-agent` | On-device sampler binary (pushed automatically, not used standalone) |

### xperf-cli

Real-time Android app performance monitor. All sampling runs **on-device** via a
resident agent daemon (`xperf-agent`), streamed back over an adb-forwarded TCP
connection as NDJSON — there is no per-round adb polling, so sampling intervals
down to ~50 ms are practical.

#### Features

- **CPU usage** (single-core scale, same convention as `adb top`: 100% = one core)
  - Process-level and thread-level usage
  - Peak tracking and process restart detection
  - Time-series charts and CSV export (millisecond timestamps)
- **Memory usage**
  - Total PSS, plus category breakdown (Java/Native/Code/Stack/Graphics/
    DMA-BUF/...) at intervals ≥ 500 ms (via on-device `dumpsys meminfo`)
  - At intervals < 500 ms memory falls back to `/proc/<pid>/smaps_rollup`
    (Pss/Rss only — `dumpsys meminfo` costs ~100 ms per call)
- **FPS** (`--fps`)
  - Per-layer frame rates from SurfaceFlinger frame timestamps — works for
    SurfaceView/game direct rendering where `gfxinfo` reports nothing
  - Multiple rendering layers are reported as separate series
  - Jank counting relative to the window's median frame interval
- **Device-level context metrics**: CPU frequency (`--freq`), thermal
  (`--thermal`), GPU busy/memory (`--gpu` / `--gpu-mem`), process IO (`--io`),
  network (`--net`)
- **Deep-dive recording**: perfetto trace + SQL analysis (`--trace N`),
  simpleperf call-stack profiling with flamegraph (`--stack N`)
- **Capture**: screenshot (`--screenshot`), screen recording via scrcpy
  (`--record N`), screen mirror (`--mirror`), logcat capture with package /
  level / regex filtering (`--logcat [--logcat-regex RE]`)
- **Verification**: threshold alerts (`--threshold`), cold-start measurement
  (`--cold-start`), baseline save/compare (`--save-baseline` /
  `--compare-baseline`)
- **SSH remote backend** (`--remote HOST`): device attached to a remote Linux
  machine — sampling, deep-dive and capture all work through an SSH tunnel.
  Finder/DMG launches resolve host `adb` from `XPERF_ADB`, Android SDK environment variables,
  the current `PATH`, and standard macOS SDK locations; `ssh` uses the same fallback strategy.
  GUI errors remain visible as `Remote connection failed (still local): ...`; the UI no longer
  replaces the original error with a second silent local fallback.
- **Multi-device**: `--device SERIAL` (per-device sessions in the GUI)
- **Data export**
  - Streaming CSV + charts under `/tmp/xperf/<package>/<timestamp>/{cpu,memory,fps,thread,...}/`

#### Requirements

- An Android device (adb reachable). **root adb is preferred**; non-root works
  with capability degradation (memory falls back to rate-limited `dumpsys`,
  process IO unavailable — see WORKSPACE.md section G for the full matrix)
- Host: Rust toolchain; Android NDK (≥ 25.1) for the agent cross-build — the
  linker is auto-detected by `.cargo/ndk-clang.sh` per host OS

#### Usage

```bash
./target/release/xperf-cli --package <package_name> [--cpu] [--memory] [--fps] [--thread] [-i <interval_ms>]
```

Options:
- `--package, -p`: Android package name to monitor
- `--cpu`: Monitor CPU usage
- `--memory`: Monitor memory usage
- `--fps`: Monitor FPS (per SurfaceFlinger layer)
- `--thread`: Monitor thread activity (requires --cpu)
- `--interval, -i`: Sampling interval in **milliseconds** (default: 1000)

The device-side agent is built (if missing) and pushed to
`/data/local/tmp/xperf-agent` automatically on first run when using the CLI.
The released GUI bundles a prebuilt agent resource and never compiles it at runtime.

Examples:
```bash
# Monitor CPU, memory and FPS at the default 1s interval
./target/release/xperf-cli --package com.example.app --cpu --memory --fps

# Fine-grained CPU burst analysis at 50 ms
./target/release/xperf-cli --package com.example.app --cpu -i 50

# Monitor only memory
./target/release/xperf-cli --package com.example.app --memory
```

#### Output Format

At intervals ≥ 500 ms each sample is printed in detail; below that a per-second
summary (avg/max) is printed instead — full detail is always in the CSV exports.

```
[20:23:18] Process CPU: 27.6% (pid: 29697)
[20:23:18] Memory Usage: 482692 KB (Java: 7328, Native: 117872, Code: 36100, Graphics: 0) [pid 29697]
[20:23:18] FPS: 30.2 (jank: 0, frames: 30, layer: SVM Container#0) [pid 29697]
```

### xperf-gui

Tauri 2 desktop GUI over the same agent transport: package picker, per-PID
CPU/memory charts and per-layer FPS charts on a live canvas.

```bash
./target/release/xperf-gui --package <package_name> --cpu --memory --fps
```

## Download

Prebuilt packages are published on the internal GitLab: [Releases](https://gitlab.chehejia.com/ligraphic/xperf/-/releases).

- `xperf-vX.Y.Z-linux-x86_64.tar.gz` — Ubuntu 20.04+ (glibc 2.31, CLI + agent)
- `xperf-vX.Y.Z-linux-x86_64-gui.AppImage` — Ubuntu 22.04+ (Tauri 2 GUI + bundled agent)
- `xperf-vX.Y.Z-macos-arm64.tar.gz` / `xperf-vX.Y.Z-macos-x86_64.tar.gz` — CLI + agent
- `xperf-vX.Y.Z-macos-arm64-gui.dmg` / `xperf-vX.Y.Z-macos-x86_64-gui.dmg` — GUI + bundled agent

CLI archives contain `xperf-cli` and `agent/xperf-agent`. GUI AppImage/DMG bundles
`agent/xperf-agent` as an application resource and never compiles it at runtime.
macOS DMGs use a complete ad-hoc bundle signature by default to seal application resources. Public distribution still requires a Developer ID signature and Apple notarization; otherwise Gatekeeper may block launch.
Release notes live in [CHANGELOG.md](CHANGELOG.md).

## Building

The project uses Cargo workspaces. To build all host tools (the agent is
Android-only and is **not** part of the default member set):

```bash
cargo build --release
```

The on-device agent cross-build (normally automatic on first run):

```bash
cargo build -p xperf-agent --target aarch64-linux-android --release
```

Host binaries are in `target/release/`; the agent binary in
`target/aarch64-linux-android/release/`.

## Tests

```bash
cargo test
```

(Host tools only — default member set. `--workspace` would hit the agent's
host-target `compile_error!` guard.)

# XTools

A collection of development tools for Android development.

## Projects

| Tool | Description |
|------|-------------|
| `xperformance` | CLI: Android app performance monitor (CPU / memory / FPS) |
| `xperf-gui` | Tauri 2 GUI: real-time charts for the same metrics |
| `xperf-agent` | On-device sampler binary (pushed automatically, not used standalone) |
| `xrm` | Safe `rm` replacement with system-path protection |

### xperformance

Real-time Android app performance monitor. All sampling runs **on-device** via a
resident agent binary (`xperf-agent`), streamed back over a single
`adb exec-out` connection as NDJSON — there is no per-round adb polling, so
sampling intervals down to ~50 ms are practical.

#### Features

- **CPU usage** (single-core scale, same convention as `adb top`: 100% = one core)
  - Process-level and thread-level usage
  - Peak tracking and process restart detection
  - Time-series charts (1920x1080) and CSV export (millisecond timestamps)
- **Memory usage**
  - Total PSS, plus category breakdown (Java/Native/Code/Stack/Graphics/...) at
    intervals ≥ 500 ms (via on-device `dumpsys meminfo`)
  - At intervals < 500 ms memory falls back to `/proc/<pid>/smaps_rollup`
    (Pss/Rss only — `dumpsys meminfo` costs ~100 ms per call)
- **FPS** (`--fps`)
  - Per-layer frame rates from SurfaceFlinger frame timestamps — works for
    SurfaceView/game direct rendering where `gfxinfo` reports nothing
  - Multiple rendering layers are reported as separate series
  - Jank counting relative to the window's median frame interval
- **Data export**
  - CSV + charts under `log/<package>/<timestamp>/{cpu,memory,fps,thread}/`

#### Requirements

- An Android device with **root** adb (`adbd` running as root — the agent reads
  other processes' `/proc` entries)
- Host: Rust toolchain; Android NDK for the agent cross-build (linker configured
  in `.cargo/config.toml`; override with `CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER`)

#### Usage

```bash
./target/release/xperformance --package <package_name> [--cpu] [--memory] [--fps] [--thread] [-i <interval_ms>]
```

Options:
- `--package, -p`: Android package name to monitor
- `--cpu`: Monitor CPU usage
- `--memory`: Monitor memory usage
- `--fps`: Monitor FPS (per SurfaceFlinger layer)
- `--thread`: Monitor thread activity (requires --cpu)
- `--interval, -i`: Sampling interval in **milliseconds** (default: 1000)

The device-side agent is built (if missing) and pushed to
`/data/local/tmp/xperf-agent` automatically on first run.

Examples:
```bash
# Monitor CPU, memory and FPS at the default 1s interval
./target/release/xperformance --package com.example.app --cpu --memory --fps

# Fine-grained CPU burst analysis at 50 ms
./target/release/xperformance --package com.example.app --cpu -i 50

# Monitor only memory
./target/release/xperformance --package com.example.app --memory
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

### xrm

Safe deletion tool: refuses to remove system-critical paths (even under sudo),
handles dangling symlinks correctly.

## Building

The project uses Cargo workspaces. To build all host tools:

```bash
cargo build --release --workspace
```

The on-device agent cross-build (normally automatic on first run):

```bash
cargo build -p xperf-agent --target aarch64-linux-android --release
```

Host binaries are in `target/release/`; the agent binary in
`target/aarch64-linux-android/release/`.

## Tests

```bash
cargo test --workspace -- --test-threads=1
```

(Tests share a global mock adb runner, so they must run single-threaded.)

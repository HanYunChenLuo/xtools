# xperf

Android app performance analysis toolkit: CLI + desktop GUI + on-device sampling agent.

[中文](README_zh.md) | **English**

## Projects

| Tool | Description |
|------|-------------|
| `xperf-cli` | CLI: Android app performance monitor (CPU / memory / FPS / GPU / trace / capture / ...) |
| `xperf-gui` | Tauri 2 GUI: real-time charts for the same metrics |
| `xperf-agent` | On-device sampler daemon (deployed automatically, not used standalone) |

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
  machine — sampling, deep-dive and capture all work through SSH tunnels
  (a single ControlMaster connection carries the adb protocol, plus one
  event-stream tunnel per device). Authentication: passwordless ssh-config
  alias, or username+IP+password (GUI form / `XPERF_SSH_PASSWORD` env var for
  the CLI — the password lives in process memory only, never on disk)
- **Feedback** (`--feedback "description"`): collects the last hour of
  tool-own evidence (session artifacts / diagnostic logs / per-device agent
  logs), self-checks, packs and uploads it to an internal GitLab issue;
  `--gitlab-login` authorizes in the browser (OAuth, SSO/2FA compatible) so
  issues are created as you — or use the `GITLAB_TOKEN` env var /
  `~/.config/xperf/gitlab-token` (PAT)
- **Update check** (`--check-update`): queries the internal GitLab for the
  latest release and prints current/latest versions, the release page URL and
  the asset list (detect + guide only, no self-update; always exits 0). The
  GUI top bar Settings menu can toggle the startup auto-check (default on) and
  run a manual check; a 🆕 badge appears when a newer release exists
- **Utility commands**: `--force-stop` (stop the app), `--clean-cache` (clear
  caches and collected data), `--update-simpleperf-scripts` (refresh
  flamegraph scripts)
- **Multi-device**: `--device SERIAL` (per-device sessions in the GUI)
- **Data export**
  - Streaming CSV + charts under `/tmp/xperf/<package>/<timestamp>/{cpu,memory,fps,thread,...}/`
    (macOS: `$TMPDIR/xperf/...`)

#### Requirements

- An Android device (adb reachable). **root adb is preferred**; non-root works
  with capability degradation (memory falls back to rate-limited `dumpsys`,
  process IO unavailable — see WORKSPACE.md section G for the full matrix)
- Host: Rust toolchain; Android NDK (≥ 25.1) for the agent cross-build — the
  linker is auto-detected by `.cargo/ndk-clang.sh` per host OS
- Optional tools, resolved as `XPERF_*` env override → `PATH` → well-known
  locations (see AGENTS.md for the full table): `scrcpy` **≥ 2** for
  mirror/record (built and verified against 4.x; older builds such as the
  Ubuntu 22.04 apt package are rejected with a clear error), `python3` for
  flamegraph rendering, and `trace_processor` for `--trace` SQL analysis
  (auto-downloaded from get.perfetto.dev on first use)

#### Usage

```bash
./target/release/xperf-cli --package <package_name> [options...]
```

Common options (full list in `--help`):

| Group | Options |
|-------|---------|
| Basics | `--package, -p` (package name), `--interval, -i` (interval in ms, default 1000, min 50), `--device, -d` (serial, required with multiple devices) |
| Metrics | `--cpu`, `--memory`, `--thread` (needs `--cpu`), `--fps`, `--freq`, `--thermal`, `--gpu`, `--gpu-mem`, `--io`, `--net` |
| Deep-dive | `--trace <seconds>` (perfetto), `--stack <seconds>` (simpleperf hotspots) |
| Verification | `--threshold <rules>`, `--cold-start <activity>`, `--save-baseline` / `--compare-baseline` |
| Capture | `--screenshot`, `--record <seconds>`, `--mirror`, `--logcat [--logcat-regex <regex>]` |
| Remote | `--remote <host>`, `--remote-adb <path>`, `--remote-adb-port <port>` |
| Feedback | `--feedback <description>`, `--gitlab-login` / `--gitlab-logout` |
| Update check | `--check-update` (always exits 0) |
| Utility | `--force-stop`, `--clean-cache`, `--update-simpleperf-scripts` |

The device-side agent is built (if missing) and pushed to
`/data/local/tmp/xperf-agent` automatically on first run when using the CLI.
The released GUI bundles a prebuilt agent resource and never compiles it at runtime.

Examples:
```bash
# Monitor CPU, memory and FPS at the default 1s interval
./target/release/xperf-cli --package com.example.app --cpu --memory --fps

# Fine-grained CPU burst analysis at 50 ms (with per-thread detail)
./target/release/xperf-cli --package com.example.app --cpu --thread -i 50

# Sample while recording a 10s perfetto trace (same window, SQL report at exit)
./target/release/xperf-cli --package com.example.app --cpu --trace 10

# Threshold alerts + save a baseline at session end (diff next time with --compare-baseline)
./target/release/xperf-cli --package com.example.app --cpu --memory \
    --threshold cpu>80,mem>500 --save-baseline

# Capture logcat (package filter + message-body regex)
./target/release/xperf-cli --package com.example.app --logcat --logcat-regex 'ANR|FATAL'

# SSH remote backend (device attached to a remote Linux machine, e.g. "myserver")
./target/release/xperf-cli --remote myserver --package com.example.app --cpu
```

#### Output Format

At intervals ≥ 500 ms each sample is printed in detail; below that a per-second
summary (avg/max) is printed instead — full detail is always in the CSV exports.

```
[20:23:18] Process CPU: 27.6% (pid: 29697)
[20:23:18] Memory Usage: 482692 KB (Java: 7328, Native: 117872, Code: 36100, Graphics: 0) [pid 29697]
[20:23:18] FPS: 60.0 (jank: 0, frames: 30, layer: SurfaceView[com.example.app](BLAST)#0) [pid 29697]
```

### xperf-gui

Tauri 2 desktop GUI over the same agent transport:

- **Multi-device in parallel**: one tab per online device in the top bar
  (hot-plug aware; a disconnected device stays greyed-out with its data,
  sampling resumes when it comes back)
- **Per-device page**:
  - Sidebar — app management (package picker / open / restart / stop the app,
    cold-start measured on open/restart), screen capture (screenshot /
    recording / mirror), data management (CSV export / save & compare
    baselines / clean cache), device permission badge with "acquire root"
  - Four sub-tabs — performance metrics (CPU/memory/FPS/GPU line charts +
    live values + top threads + peak/baseline/cold-start panels), Perfetto
    analysis, Simpleperf hotspots (browser flamegraph), logs (logcat live
    view with level/package/regex filtering, hot-switchable)
- **SSH remote connection**: top-bar dropdown (with a username+IP+password
  form)
- **Feedback**: one-click collect-and-upload to a GitLab issue
- Dark/light theme; `--package --device` command-line args auto-start sampling

```bash
./target/release/xperf-gui --package <package_name> --cpu --memory --fps
```

## For AI agents

`xperf-cli` is fully non-interactive and agent-friendly: give it a bounded
window and consume the artifacts.

```bash
# Bounded sampling → exit 0 after 30 s; CSVs under /tmp/xperf/<pkg>/<ts>/ (macOS: $TMPDIR/xperf/...)
xperf-cli --package com.example.app --cpu --memory --fps --duration 30

# Regression assertion — verdict in <session>/baseline_report.txt
xperf-cli --package com.example.app --cpu --memory --duration 30 --compare-baseline
```

Exit codes: `0` success (threshold alerts and baseline regressions are
report-only and never fail the run), `1` runtime failure, `2` argument error.
Every sampling session writes `<session>/summary.json` on exit — structured
metrics plus threshold/baseline verdicts, the assertion entry point for CI
(no terminal-output parsing needed).
Keep `agent/xperf-agent` next to the `xperf-cli` binary (release tarball
layout) or set `XPERF_AGENT_BIN`.

Full recipes, CSV schemas and pitfalls: [AGENTS.md](AGENTS.md)
(Codex/auto-discovered) and [skills/xperf/SKILL.md](skills/xperf/SKILL.md)
(Claude Code / siada skill format).

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

See [CONTRIBUTING.md](CONTRIBUTING.md) for environment setup and workflow conventions.

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

## License

[MIT](LICENSE)

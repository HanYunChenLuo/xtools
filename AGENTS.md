# AGENTS.md — xperf for coding agents

Guidance for AI agents (Codex / Claude Code / etc.) that drive `xperf-cli` from
the shell to collect Android performance data and assert on it. For repository
internals see `CLAUDE.md`; this file is about **using** the tool.

## Install / locate the binary

Release archives (internal GitLab Releases page) contain exactly two files —
keep them together:

```
xperf-cli            # host CLI
agent/xperf-agent    # on-device sampler (aarch64 Android, pushed automatically)
```

Resolution order: `XPERF_AGENT_BIN` env override → `agent/xperf-agent` next to
the `xperf-cli` binary → dev-checkout auto cross-build (needs Rust + NDK).
No NDK or extra setup is needed when using the tarball as-is. Do not move
`xperf-cli` out of its directory without also moving `agent/`.

Prerequisites on the host: `adb` in PATH (or reachable via `--remote`), an
adb-connected Android device. root adb preferred; non-root works with
degradation (memory rate-limited to ≥500 ms, process IO disabled).

## Core recipes

All commands are non-interactive and bounded when `--duration` is given —
always prefer bounded runs in scripts.

```bash
# Bounded sampling: CPU + memory + FPS for 30 s, then exit 0
xperf-cli --package com.example.app --cpu --memory --fps --duration 30

# Threshold assertion (report at exit; see "Exit codes" — alerts do NOT
# change the exit code, parse the report or the CSVs)
xperf-cli --package com.example.app --cpu --memory --duration 60 \
    --threshold 'cpu>80,mem>500,fps<30'

# Regression check: save baseline once, compare after a change
xperf-cli --package com.example.app --cpu --memory --fps --duration 30 --save-baseline
xperf-cli --package com.example.app --cpu --memory --fps --duration 30 --compare-baseline
#   → verdict printed and written to <session>/baseline_report.txt

# Deep dives (same bounded window as sampling when combined)
xperf-cli --package com.example.app --cpu --trace 10   # perfetto + SQL report
xperf-cli --package com.example.app --cpu --stack 10   # simpleperf hotspots

# Capture (screenshot/record usable standalone, no --package needed)
xperf-cli --package com.example.app --screenshot
xperf-cli --record 15                                  # screen recording (mp4)
# logcat standalone runs until Ctrl-C — combine with sampling for a bounded window
xperf-cli --package com.example.app --cpu --logcat --logcat-regex 'ANR|FATAL' --duration 30

# Cold-start measurement (standalone, self-bounded by a 15s timeout;
# also works alongside sampling to feed the session summary)
xperf-cli --package com.example.app --cold-start .MainActivity

# Multiple devices attached: --device is mandatory
xperf-cli --device <serial> --package com.example.app --cpu --duration 10

# Device attached to a remote Linux machine
xperf-cli --remote <ssh-host> --package com.example.app --cpu --duration 10
```

The app must be **running** during sampling — if no process matches the
package the session produces no CSVs (exit code is still 0); `summary.json`
is still written with `samples: 0` and null metrics. Use `--cold-start` or
`adb shell am start` first when in doubt.

## Output layout

Data root: `/tmp/xperf` (Linux) / `$TMPDIR/xperf` (macOS). Each sampling
session writes to `<root>/<package>/<YYYYMMDD_HHMMSS>/` (the path is printed
on stdout as `Created timestamp directory: ...`; or take the newest dir under
`<root>/<package>/`). GUI sessions use a `<ts>-<serial>` suffix.

| Path | Content |
|------|---------|
| `cpu/cpu_<pid>_data.csv` | `Timestamp,Process CPU (%)` — single-core scale (100% = one core, can exceed 100%) |
| `memory/memory_<pid>_data.csv` | `Timestamp,Total PSS (MB),Java Heap (MB),Native Heap (MB),Code (MB),Stack (MB),Graphics (MB),DMA-BUF (MB),Other (MB),System (MB)` |
| `fps/<pkg>_fps_data_pid<pid>.csv` | `Timestamp,FPS,Jank,Layer` — one file per PID, one row per active layer |
| `thread/thread_<name>_<tid>_<pid>.csv` | `Timestamp,CPUUsage` (only with `--cpu --thread`) |
| `freq/freq_data.csv` | `Timestamp,cpu0 (MHz),...` per-core frequencies |
| `thermal/thermal_data.csv` | `Timestamp,Status,Sensor,TempC` (long format) |
| `gpu/gpu_data.csv` | `Timestamp,Busy (%),Util (%),Clock (MHz),Max Clock (MHz)` |
| `gpumem/gpumem_<pid>_data.csv` | `Timestamp,Process GPU Mem (MB),Global GPU Mem (MB)` |
| `io/io_<pid>_data.csv` | `Timestamp,Read (KB/s),Write (KB/s),Disk Read (KB/s),Disk Write (KB/s)` |
| `net/net_data.csv` | `Timestamp,RX (KB/s),TX (KB/s)` — whole-device counters |
| `trace/` | `*.pftrace` + `trace_analysis.txt` (SQL report) + `trace_queries.sql` |
| `stack/` | `stack_*.data` + `simpleperf_report.txt` (threads / self / children views) |
| `capture/` | `shot_*.png`, `record_*.mp4` |
| `logcat/logcat.log` | `-v threadtime -v year` lines, same device clock as CSV timestamps |
| `baseline_report.txt` | verdict per metric (only with `--compare-baseline`) |
| `summary.json` | structured session summary (always written at sampling exit — see below) |
| `markers.csv` | timeline markers (only if CLI socket markers were sent) |

### `summary.json` — structured session summary

Written at every sampling session exit (even with zero samples). Top level is
the exact `SessionSummary` schema of the baseline JSON (`version`, `package`,
`duration_s`, `samples`, `pids`, `restarts`, `cold_start_ms`, per-metric
`{avg, max, count}` blocks — same-session values are identical to a baseline
saved with `--save-baseline`), plus two verdict objects:

- `thresholds` (`null` unless `--threshold` given):
  `{all_pass, rules: [{rule, pass, triggers, extreme, last_trigger}]}`
- `baseline` (`null` unless `--save-baseline`/`--compare-baseline` given):
  `{action, baseline_file, report_file, outcome, error}` — `action` is one of
  `saved` / `compared` / `skipped_no_data` / `no_baseline` / `failed`; `outcome`
  (only when `compared`) is `{verdict, regressions, improvements, flat,
  no_compare, regressed_metrics}` with `verdict` ∈ `no_regression` /
  `regression` / `no_comparable`.

CSVs are streamed row-by-row with flush; a killed run loses at most the tail.
Timestamps are local-time with millisecond precision; logcat uses the device
clock — they share the timeline on the device.

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success. Includes: bounded/Ctrl-C sampling end; threshold alerts triggered; baseline regression found (both are **report-only**, they never fail the run) |
| 1 | Runtime failure: invalid/missing package name; multi-device without `--device`; `--device` offline; remote init failure; sampling/agent-deploy failure; **standalone** `--trace`/`--stack` recording failure; standalone `--screenshot`/`--record`/`--logcat`/`--mirror`/`--cold-start` failure; `--force-stop` / `--feedback` / `--gitlab-login` failure |
| 2 | CLI argument parse error (unknown flag, bad value, `--save-baseline` + `--compare-baseline` together, `--logcat-regex` without `--logcat`) |

Key nuance: in **standalone** mode a failing capability exits 1; when the same
capability runs **in parallel with sampling** its failure only prints a warning
(sampling output is the primary product). Analysis failures (trace SQL,
simpleperf report rendering) never affect the exit code — the raw data was
already pulled.

To assert on performance in CI, do not rely on the exit code alone — read
`summary.json` (structured: `.thresholds.all_pass`, `.baseline.outcome.verdict`),
or parse the threshold verification report (stdout) / `baseline_report.txt`,
or diff the CSVs directly.

## Common pitfalls

- **Multiple devices**: everything fails with `more than one device` unless
  `--device <serial>` is given (serials from `adb devices`).
- **First run in a dev checkout** cross-builds the agent (~1–2 min, needs
  Android NDK ≥ 25.1); the tarball install never builds anything.
- **Non-root devices**: memory detail falls back to ≥500 ms `dumpsys`, process
  IO is disabled (an `err` event is emitted), simpleperf needs a debuggable
  app. Other metrics work.
- **`--duration` only bounds sampling sessions.** Mirror-only runs until
  Ctrl-C/window close, pure deep-dive/recording are self-bounded by their own
  seconds argument, standalone `--logcat` runs until Ctrl-C, standalone
  `--cold-start` is self-bounded by its 15 s measurement timeout.
- `--save-baseline` / `--compare-baseline` need at least one sampling metric
  flag; the comparison needs identical scenarios to be meaningful.
- The terminal output at intervals < 500 ms is a per-second aggregate — always
  consume the CSVs for full resolution.

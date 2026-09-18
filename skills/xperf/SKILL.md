---
name: xperf
description: "Android 应用性能采集与回归断言（xperf-cli）。当需要测量 Android 应用的 CPU/内存/FPS/GPU/IO/网络，做性能回归对比（基线 diff）、阈值断言、perfetto 调度深挖、simpleperf 函数热点、冷启动测量、截屏/录屏/logcat 抓取时使用。Use when asked to profile or monitor an Android app's performance, assert performance budgets, or investigate jank/CPU hotspots from the command line."
metadata:
  requires:
    bins: ["xperf-cli", "adb"]
  cliHelp: "xperf-cli --help"
---

# xperf — Android performance sampling from the shell

`xperf-cli` samples an Android app's CPU / memory / FPS / GPU / IO / network
via an on-device agent (no adb polling), streams CSVs to disk, and provides
verification primitives (thresholds, baselines, perfetto/simpleperf deep
dives). Everything is non-interactive and scriptable.

## When to use

- "Profile app X for 30s and tell me CPU/memory/FPS"
- "Assert CPU stays below N% / memory below N MB during scenario Y"
- "Did this change regress performance?" (baseline save → compare)
- "Why is it janky / where is CPU time spent?" (`--trace` / `--stack`)
- "Capture a screenshot / screen recording / logcat around the issue"

## Prerequisites

- `adb` reachable device; root preferred (non-root: memory detail rate-limited
  to ≥500 ms, per-process IO disabled, simpleperf needs debuggable app).
- Multiple devices attached → `--device <serial>` is **mandatory**.
- Device on a remote Linux host → prefix with `--remote <ssh-host>`.
- The target app must be **running** during sampling, or no data is produced.

## Command recipes (always prefer bounded runs)

```bash
# Bounded sampling — exits 0 after N seconds
xperf-cli --package <pkg> --cpu --memory --fps --duration 30

# Threshold assertion — exit report lists violations (does NOT change exit code)
xperf-cli --package <pkg> --cpu --memory --duration 60 --threshold 'cpu>80,mem>500'

# Regression check — two runs of the same scenario
xperf-cli --package <pkg> --cpu --memory --fps --duration 30 --save-baseline
# ... change build, re-run:
xperf-cli --package <pkg> --cpu --memory --fps --duration 30 --compare-baseline
# verdict: <session>/baseline_report.txt (⚠ = regression, ±10% + floor gates)

# Why is CPU high — perfetto scheduling trace, SQL report at exit
xperf-cli --package <pkg> --cpu --trace 10

# Which function burns CPU — simpleperf call stacks (threads/self/children)
xperf-cli --package <pkg> --cpu --stack 10

# Cold start (standalone, self-bounded by a 15s timeout)
xperf-cli --package <pkg> --cold-start .MainActivity

# Capture
xperf-cli --package <pkg> --screenshot
xperf-cli --record 15
# logcat standalone runs until Ctrl-C — combine with sampling for a bounded window
xperf-cli --package <pkg> --cpu --logcat --logcat-regex 'ANR|FATAL' --duration 30
```

## Consuming the output

Session dir: `/tmp/xperf/<pkg>/<YYYYMMDD_HHMMSS>/` (macOS: `$TMPDIR/xperf/...`;
printed on stdout as `Created timestamp directory: ...`, else newest dir under
`/tmp/xperf/<pkg>/`).

- `cpu/cpu_<pid>_data.csv` — `Timestamp,Process CPU (%)`, single-core scale
  (100% = one busy core; multi-thread can exceed 100%)
- `memory/memory_<pid>_data.csv` — PSS total + category breakdown, **MB**
- `fps/<pkg>_fps_data_pid<pid>.csv` — `Timestamp,FPS,Jank,Layer`; FPS 0 = idle
  screen, not a freeze
- `trace/trace_analysis.txt` — thread CPU / wakeup latency / per-core busy /
  frame timeline; `stack/simpleperf_report.txt` — hotspot functions
- `capture/shot_*.png`, `capture/record_*.mp4`, `logcat/logcat.log`
- Full table (freq/thermal/gpu/gpumem/io/net/thread CSVs): see repo
  `AGENTS.md` § Output layout.

## Exit codes (assert correctly)

- **0** = success — *including* triggered thresholds and detected regressions
  (report-only). Parse `baseline_report.txt` / the threshold report, or the
  CSVs, to assert.
- **1** = runtime failure (bad package/device/remote, sampling or standalone
  recording/capture failure).
- **2** = CLI argument error.

A capability that fails **alongside** sampling only warns; the same failure
**standalone** exits 1.

## Pitfalls

- Interval < 500 ms prints a per-second aggregate — full resolution is in CSVs.
- `--save-baseline`/`--compare-baseline` require ≥1 metric flag and matching
  scenarios.
- First run from a dev checkout cross-builds the agent (~1–2 min, NDK ≥ 25.1);
  release tarballs ship it prebuilt next to `xperf-cli` (`agent/` dir must
  stay beside the binary, or set `XPERF_AGENT_BIN`).

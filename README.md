# XTools

A collection of development tools for Android development.

## Projects

### xperformance

A real-time Android app performance monitoring tool that tracks CPU and memory usage through ADB.

#### Features

- Real-time CPU usage monitoring
  - Process-specific CPU usage
  - System-wide CPU usage and idle state
  - Thread count tracking
  - Detailed thread-level CPU usage in verbose mode
- Memory usage monitoring
  - Total PSS tracking
  - Detailed memory breakdown in verbose mode
- Process monitoring
  - Automatic process restart detection
  - Peak usage tracking
  - Process start time logging
- ADB connection monitoring
  - Automatic termination on connection loss
- Detailed logging
  - Timestamp-based logging
  - Formatted and aligned output
  - Comprehensive performance metrics

#### Usage

```bash
./target/release/xperformance --package <package_name> [--cpu] [--memory] [-i <interval>] [--verbose]
```

Options:
- `--package, -p`: Android package name to monitor
- `--cpu`: Monitor CPU usage
- `--memory`: Monitor memory usage
- `--interval, -i`: Sampling interval in seconds (default: 1)
- `--verbose, -v`: Enable verbose output with detailed metrics

Examples:
```bash
# Monitor both CPU and memory with verbose output
./target/release/xperformance --package com.example.app --cpu --memory --verbose

# Monitor only CPU with 2-second interval
./target/release/xperformance --package com.example.app --cpu -i 2

# Monitor only memory with verbose output
./target/release/xperformance --package com.example.app --memory --verbose
```

#### Output Format

The tool provides formatted output with timestamps:

```
[14:59:48] Process: 3.3%, System: 180.0% (idle: 610.0%, pid: 25786, threads: 90)
[14:59:49] Memory Usage: 256.5 MB
[14:59:53] Peak CPU: 6.6% at 14:59:49

[14:59:53] Process restarted! New PID: 25786 (previous: 25245), Start time: 2024-12-31 14:59:53
```

Detailed metrics are saved in the `log` directory when running in verbose mode.

## Building

The project uses Cargo workspaces to manage all tools. To build all tools:

```bash
cargo build --release
```

The compiled binaries will be available in the `target/release` directory.
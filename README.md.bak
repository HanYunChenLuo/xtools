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
  - Detailed thread-level CPU usage
  - CPU usage time-series chart generation with high resolution (1920x1080)
  - Thread CPU usage visualization with time-series charts
- Memory usage monitoring
  - Total PSS tracking
  - Detailed memory breakdown (Java Heap, Native Heap, Code, Stack, Graphics, etc.)
  - Memory composition charts with high resolution (1920x1080)
  - Individual memory metrics visualization
- Process monitoring
  - Absolute time-based sampling for consistent intervals
  - Automatic process restart detection
  - Peak usage tracking
  - Process start time logging
- Data visualization and export
  - High resolution charts (1920x1080) for better detail
  - CSV export of all collected metrics
  - Organized output in timestamp-based directories
  - Separate directories for CPU, memory, and thread data
- ADB connection monitoring
  - Automatic termination on connection loss
- Performance optimizations
  - Skip delayed samples to maintain consistent sampling frequency
  - Minimized filesystem overhead

#### Usage

```bash
./target/release/xperformance --package <package_name> [--cpu] [--memory] [--thread] [-i <interval>]
```

Options:
- `--package, -p`: Android package name to monitor
- `--cpu`: Monitor CPU usage
- `--memory`: Monitor memory usage
- `--thread`: Monitor thread activity (requires --cpu)
- `--interval, -i`: Sampling interval in seconds (default: 1)

Examples:
```bash
# Monitor CPU, memory and thread activity
./target/release/xperformance --package com.example.app --cpu --memory --thread

# Monitor only CPU with 2-second interval
./target/release/xperformance --package com.example.app --cpu -i 2

# Monitor only memory
./target/release/xperformance --package com.example.app --memory
```

#### Output Format

The tool provides formatted output with timestamps:

```
[14:59:48] Process CPU: 3.3% (pid: 25786)
[14:59:48] Memory Usage: 262144 KB (Java: 5092, Native: 105720, Code: 28672, Graphics: 32768)
[14:59:53] Peak CPU: 6.6% at 2023-12-31 14:59:49
```

Data is saved in the `log/<package_name>/<timestamp>/` directory with the following structure:
- `cpu/`: CPU charts and CSV data
- `memory/`: Memory charts and CSV data
- `thread/`: Thread activity charts and CSV data

## Building

The project uses Cargo workspaces to manage all tools. To build all tools:

```bash
cargo build --release
```

The compiled binaries will be available in the `target/release` directory.
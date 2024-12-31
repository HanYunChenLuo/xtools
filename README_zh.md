# XTools

Android 开发工具集合。

## 项目列表

### xperformance

一个基于 ADB 的实时 Android 应用性能监控工具，用于跟踪 CPU 和内存使用情况。

#### 功能特性

- 实时 CPU 使用率监控
  - 进程级 CPU 使用率
  - 系统级 CPU 使用率和空闲状态
  - 线程数量跟踪
  - 日志中详细的线程级 CPU 使用情况
- 内存使用监控
  - 总 PSS 跟踪
  - 日志中详细的内存分布
- 进程监控
  - 自动检测进程重启
  - 峰值使用跟踪
  - 进程启动时间记录
- ADB 连接监控
  - 连接丢失时自动终止
- 详细日志记录
  - 基于时间戳的日志
  - 格式化对齐的输出
  - 全面的性能指标

#### 使用方法

```bash
./target/release/xperformance --package <包名> [--cpu] [--memory] [-i <间隔>]
```

选项：
- `--package, -p`：要监控的 Android 包名
- `--cpu`：监控 CPU 使用率
- `--memory`：监控内存使用情况
- `--interval, -i`：采样间隔（秒），默认为 1

示例：
```bash
# 同时监控 CPU 和内存
./target/release/xperformance --package com.example.app --cpu --memory

# 每 2 秒监控一次 CPU
./target/release/xperformance --package com.example.app --cpu -i 2

# 仅监控内存
./target/release/xperformance --package com.example.app --memory
```

#### 输出格式

工具提供带时间戳的格式化输出：

```
[14:59:48] Process: 3.3%, System: 180.0% (idle: 610.0%, pid: 25786, threads: 90)
[14:59:49] Memory Usage: 256.5 MB
[14:59:53] Peak CPU: 6.6% at 14:59:49

[14:59:53] Process restarted! New PID: 25786 (previous: 25245), Start time: 2024-12-31 14:59:53
```

详细的性能指标日志保存在 `log` 目录中。

## 构建

项目使用 Cargo 工作空间管理所有工具。构建所有工具：

```bash
cargo build --release
```

编译后的二进制文件将位于 `target/release` 目录中。
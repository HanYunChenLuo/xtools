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
  - 详细的线程级 CPU 使用情况
  - 高分辨率 (1920x1080) CPU 使用率时间序列图表
  - 线程 CPU 使用率可视化时间序列图表
- 内存使用监控
  - 总 PSS 跟踪
  - 详细内存分布（Java 堆、Native 堆、代码、栈、图形等）
  - 高分辨率 (1920x1080) 内存组成图表
  - 单独内存指标可视化
- 进程监控
  - 基于绝对时间的采样，确保一致的采样间隔
  - 自动检测进程重启
  - 峰值使用跟踪
  - 进程启动时间记录
- 数据可视化与导出
  - 高分辨率图表 (1920x1080)，提供更清晰的细节
  - 所有收集指标的 CSV 导出
  - 基于时间戳的有组织输出目录
  - CPU、内存和线程数据的单独目录
- ADB 连接监控
  - 连接丢失时自动终止
- 性能优化
  - 跳过延迟的采样以保持一致的采样频率
  - 最小化文件系统开销

#### 使用方法

```bash
./target/release/xperformance --package <包名> [--cpu] [--memory] [--thread] [-i <间隔>]
```

选项：
- `--package, -p`：要监控的 Android 包名
- `--cpu`：监控 CPU 使用率
- `--memory`：监控内存使用情况
- `--thread`：监控线程活动（需要 --cpu 选项）
- `--interval, -i`：采样间隔（秒），默认为 1

示例：
```bash
# 同时监控 CPU、内存和线程活动
./target/release/xperformance --package com.example.app --cpu --memory --thread

# 每 2 秒监控一次 CPU
./target/release/xperformance --package com.example.app --cpu -i 2

# 仅监控内存
./target/release/xperformance --package com.example.app --memory
```

#### 输出格式

工具提供带时间戳的格式化输出：

```
[14:59:48] Process CPU: 3.3% (pid: 25786)
[14:59:48] Memory Usage: 262144 KB (Java: 5092, Native: 105720, Code: 28672, Graphics: 32768)
[14:59:53] Peak CPU: 6.6% at 2023-12-31 14:59:49
```

数据保存在 `log/<包名>/<时间戳>/` 目录下，具有以下结构：
- `cpu/`：CPU 图表和 CSV 数据
- `memory/`：内存图表和 CSV 数据
- `thread/`：线程活动图表和 CSV 数据

## 构建

项目使用 Cargo 工作空间管理所有工具。构建所有工具：

```bash
cargo build --release
```

编译后的二进制文件将位于 `target/release` 目录中。
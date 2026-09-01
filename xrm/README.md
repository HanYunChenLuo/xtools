# xrm - 安全的文件删除工具

一个用 Rust 编写的安全文件删除命令行工具，专门设计用于防止意外的系统破坏性操作。

## 🔒 安全特性

- **拒绝危险操作**: 自动检测并拒绝执行 `sudo xrm /` 和 `sudo xrm /*` 等危险命令
- **系统目录保护**: 防止删除关键系统目录（如 `/bin`, `/usr`, `/etc` 等）
- **权限检测**: 智能检测 sudo 权限并在危险操作时给出警告
- **路径规范化**: 使用规范路径防止符号链接绕过安全检查

## 📦 安装

### 方法一：使用安装脚本
```bash
cd xrm
chmod +x install.sh
./install.sh
```

### 方法二：手动编译安装
```bash
cd xrm
cargo build --release
sudo cp target/release/xrm /usr/local/bin/
```

## 🚀 使用方法

### 基本语法
```bash
xrm [选项] <文件或目录>...
```

### 选项
- `-f, --force`: 强制删除，忽略不存在的文件
- `-v, --verbose`: 显示详细信息
- `-h, --help`: 显示帮助信息
- `-V, --version`: 显示版本信息

### 使用示例

#### 删除单个文件
```bash
xrm file.txt
```

#### 删除多个文件
```bash
xrm file1.txt file2.txt file3.txt
```

#### 删除目录（自动递归）
```bash
xrm directory/
```

#### 强制删除（忽略不存在的文件）
```bash
xrm -f nonexistent.txt
```

#### 详细模式
```bash
xrm -v old_project/
```

输出示例：
```
🗑️  xrm - 安全文件删除工具
强制模式: 关闭
要删除的项目: ["old_project/"]

🗑️  递归删除目录: old_project/

✅ 成功删除: 1 个项目
```

## ⚠️ 安全限制

### 被拒绝的危险操作
以下命令会被自动拒绝执行：

```bash
sudo xrm /          # 删除根目录
sudo xrm /*         # 删除根目录下所有内容
sudo xrm /.*        # 删除根目录下隐藏文件
```

### 受保护的系统目录
以下系统关键目录受到保护，无法删除：
- `/` (根目录)
- `/bin` (系统二进制文件)
- `/boot` (启动文件)
- `/dev` (设备文件)
- `/etc` (系统配置)
- `/lib`, `/lib64` (系统库)
- `/proc` (进程信息)
- `/root` (root 用户目录)
- `/sbin` (系统管理二进制文件)
- `/sys` (系统信息)
- `/usr` (用户程序)
- `/var` (变量数据)

### 错误示例
```bash
$ sudo xrm /
❌ 错误: 拒绝执行危险的根目录删除操作!
   为了系统安全，禁止使用 sudo 执行以下操作:
   - sudo xrm /
   - sudo xrm /*
   - sudo xrm /.*
```

## 🧪 测试

运行单元测试：
```bash
cargo test
```

测试输出：
```
running 2 tests
test tests::test_dangerous_operation_detection ... ok
test tests::test_system_critical_path ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## 🔧 开发

### 项目结构
```
xrm/
├── Cargo.toml          # 项目配置
├── src/
│   └── main.rs         # 主程序代码
├── install.sh          # 安装脚本
└── README.md           # 说明文档
```

### 依赖项
- `clap`: 命令行参数解析

### 构建
```bash
cargo build --release
```

## 📝 许可证

本项目采用开源许可证，详见 LICENSE 文件。

## 🤝 贡献

欢迎提交 Issue 和 Pull Request 来改进这个工具！

## ⚡ 性能

- 快速启动：使用 Rust 编写，启动时间极短
- 内存安全：Rust 的内存安全保证
- 跨平台：支持 Linux、macOS 和 Windows

## 🔍 与 rm 命令的区别

| 特性 | rm | xrm |
|------|----|----|
| 基本删除 | ✅ | ✅ |
| 递归删除 | ✅ | ✅ |
| 强制删除 | ✅ | ✅ |
| 危险操作保护 | ❌ | ✅ |
| 系统目录保护 | ❌ | ✅ |
| sudo 检测 | ❌ | ✅ |
| 中文界面 | ❌ | ✅ |
| 详细反馈 | 部分 | ✅ |

xrm 是 rm 的安全替代品，在保持相同功能的同时增加了重要的安全保护机制。

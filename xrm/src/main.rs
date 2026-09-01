use clap::{Arg, Command};
use std::env;
use std::fs;
use std::path::Path;
use std::process;

fn main() {
    let matches = Command::new("xrm")
        .version("0.1.0")
        .about("安全的文件删除工具")
        .arg(
            Arg::new("force")
                .short('f')
                .long("force")
                .help("强制删除，忽略不存在的文件")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("verbose")
                .short('v')
                .long("verbose")
                .help("显示详细信息")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            Arg::new("files")
                .help("要删除的文件或目录")
                .required(true)
                .num_args(1..),
        )
        .get_matches();

    // 检查是否以 sudo 权限运行危险命令
    if is_running_with_sudo() {
        let files: Vec<&String> = matches.get_many::<String>("files").unwrap().collect();
        if is_dangerous_operation(&files) {
            eprintln!("❌ 错误: 拒绝执行危险的根目录删除操作!");
            eprintln!("   为了系统安全，禁止使用 sudo 执行以下操作:");
            eprintln!("   - sudo xrm /");
            eprintln!("   - sudo xrm /*");
            eprintln!("   - sudo xrm /.*");
            process::exit(1);
        }
    }

    let force = matches.get_flag("force");
    let verbose = matches.get_flag("verbose");
    let files: Vec<&String> = matches.get_many::<String>("files").unwrap().collect();

    if verbose {
        println!("🗑️  xrm - 安全文件删除工具");
        println!("强制模式: {}", if force { "开启" } else { "关闭" });
        println!("要删除的项目: {:?}", files);
        println!();
    }

    let mut success_count = 0;
    let mut error_count = 0;

    for file_path in files {
        match remove_item(file_path, force, verbose) {
            Ok(_) => success_count += 1,
            Err(e) => {
                eprintln!("❌ 删除失败 '{}': {}", file_path, e);
                error_count += 1;
            }
        }
    }

    if verbose || error_count > 0 {
        println!();
        println!("✅ 成功删除: {} 个项目", success_count);
        if error_count > 0 {
            println!("❌ 删除失败: {} 个项目", error_count);
        }
    }

    if error_count > 0 {
        process::exit(1);
    }
}

fn is_running_with_sudo() -> bool {
    // 检查是否以 sudo 权限运行
    env::var("SUDO_USER").is_ok() || env::var("SUDO_UID").is_ok()
}

fn is_dangerous_operation(files: &[&String]) -> bool {
    for file in files {
        let path = file.trim();
        // 检查危险的根目录通配符操作
        if path == "/" || path == "/*" || path == "/.*" {
            return true;
        }
        // 复用 is_system_critical_path 保证检查列表一致
        if is_system_critical_path(path) {
            return true;
        }
    }
    false
}

fn remove_item(path: &str, force: bool, verbose: bool) -> Result<(), Box<dyn std::error::Error>> {
    let path_obj = Path::new(path);

    // 对符号链接单独处理（包括悬空符号链接，exists() 对其返回 false）
    if path_obj.is_symlink() {
        // 符号链接本身无需 canonicalize，直接安全检查后删除
        if is_system_critical_path(path) {
            return Err(format!("拒绝删除系统关键路径: {}", path).into());
        }
        if verbose {
            println!("🗑️  删除符号链接: {}", path);
        }
        fs::remove_file(path_obj)?;
        return Ok(());
    }

    // 检查文件/目录是否存在
    if !path_obj.exists() {
        if force {
            if verbose {
                println!("⚠️  跳过不存在的项目: {}", path);
            }
            return Ok(());
        } else {
            return Err(format!("文件或目录不存在: {}", path).into());
        }
    }

    // 额外的安全检查：防止删除重要系统目录（通过 canonicalize 解析真实路径）
    let canonical_path = path_obj.canonicalize()?;
    let canonical_str = canonical_path.to_string_lossy();

    if is_system_critical_path(&canonical_str) {
        return Err(format!("拒绝删除系统关键路径: {}", canonical_str).into());
    }

    if path_obj.is_file() {
        if verbose {
            println!("🗑️  删除文件: {}", path);
        }
        fs::remove_file(path_obj)?;
    } else if path_obj.is_dir() {
        // 自动递归删除目录
        if verbose {
            println!("🗑️  递归删除目录: {}", path);
        }
        fs::remove_dir_all(path_obj)?;
    }

    Ok(())
}

fn is_system_critical_path(path: &str) -> bool {
    let critical_paths = [
        "/",
        "/bin",
        "/boot",
        "/dev",
        "/etc",
        "/lib",
        "/lib64",
        "/proc",
        "/root",
        "/sbin",
        "/sys",
        "/usr",
        "/var",
    ];

    for critical in &critical_paths {
        if path == *critical || path.starts_with(&format!("{}/", critical)) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dangerous_operation_detection() {
        assert!(is_dangerous_operation(&[&"/".to_string()]));
        assert!(is_dangerous_operation(&[&"/*".to_string()]));
        assert!(is_dangerous_operation(&[&"/.*".to_string()]));
        assert!(is_dangerous_operation(&[&"/bin".to_string()]));
        assert!(!is_dangerous_operation(&[&"/home/user/test".to_string()]));
    }

    #[test]
    fn test_system_critical_path() {
        assert!(is_system_critical_path("/"));
        assert!(is_system_critical_path("/bin"));
        assert!(is_system_critical_path("/usr/bin"));
        assert!(!is_system_critical_path("/home/user"));
        assert!(!is_system_critical_path("/tmp/test"));
    }
}

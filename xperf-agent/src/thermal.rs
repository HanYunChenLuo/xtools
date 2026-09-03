//! 温度/热降频采样：dumpsys thermalservice 优先，sysfs thermal zones 兜底
//! （如 SS2MAX：thermalservice 的 sensors 列表为空但 HAL 有数据）。

use crate::{dumpsys, emit, json_escape};

/// 解析 dumpsys thermalservice：
/// - "Thermal Status: N" → 热降频状态（Android ThermalStatus 0-6）
/// - "Temperature{mValue=30.8, mType=3, mName=..., mStatus=0}" → 各传感器
///
/// 输出含 Cached/Current HAL 两个温度区块（HAL 在后且更准），后者覆盖前者。
fn parse_thermalservice(out: &str) -> (Option<i32>, Vec<(String, i32, f32)>) {
    let mut status = None;
    let mut sensors: Vec<(String, i32, f32)> = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Thermal Status:") {
            status = rest.trim().parse().ok();
        } else if line.contains("temperatures") {
            sensors.clear(); // 新温度区块（Cached → Current HAL），保留最后一个
        } else if let Some(start) = line.find("Temperature{") {
            if let Some(s) = parse_temperature_entry(&line[start..]) {
                sensors.push(s);
            }
        }
    }
    (status, sensors)
}

/// 解析 "Temperature{mValue=30.8, mType=3, mName=test temperature sensor, mStatus=0}"
/// 名字可含空格/逗号，以 ", mStatus=" 为右边界。
fn parse_temperature_entry(s: &str) -> Option<(String, i32, f32)> {
    let value = s.split("mValue=").nth(1)?.split(',').next()?.trim().parse().ok()?;
    let type_ = s.split("mType=").nth(1)?.split(',').next()?.trim().parse().ok()?;
    let name_start = s.find("mName=")? + "mName=".len();
    let name_end = s.rfind(", mStatus=").unwrap_or(s.len());
    let name = s.get(name_start..name_end.min(s.len()))?.trim().to_string();
    Some((name, type_, value))
}

/// 读取 sysfs thermal zones（兜底：thermalservice 无数据时用，如 SS2MAX）。
/// /sys/class/thermal/thermal_zoneN/{type,temp}：type=传感器名，temp=millidegree Celsius。
/// 走 shell cat（SELinux 下 agent 可能无法直接 open sysfs，但 shell 命令可以）。
fn read_sysfs_thermal_zones() -> (Option<i32>, Vec<(String, i32, f32)>) {
    let cmd = "for z in /sys/class/thermal/thermal_zone*; do echo \"$(cat $z/type 2>/dev/null) $(cat $z/temp 2>/dev/null)\"; done";
    let out = match std::process::Command::new("sh")
        .args(["-c", cmd])
        .output() {
        Ok(o) => o,
        Err(_) => return (None, Vec::new()),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut sensors = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        // "aoss0-usr 39000" → ("aoss0-usr", 39.0)
        let mut parts = line.rsplitn(2, ' ');
        let temp_str = match parts.next() { Some(s) => s, None => continue };
        let name = match parts.next() { Some(s) => s.to_string(), None => continue };
        if let Ok(milli) = temp_str.parse::<i64>() {
            sensors.push((name, 0, milli as f32 / 1000.0));
        }
    }
    if sensors.is_empty() { return (None, sensors); }
    (Some(0), sensors)
}

/// 一轮温度采样：thermalservice 优先（sensors 非空才算有数据），sysfs 兜底。
/// 有数据 emit temp 事件并返回 true；无数据返回 false（由调用方一次性 err 告警）。
pub(crate) fn sample(ts: u64) -> bool {
    let from_thermal = dumpsys(&["thermalservice"]).as_deref().map(parse_thermalservice);
    let (status, sensors) = match from_thermal {
        // thermalservice 有数据才走（sensors 非空），否则走 sysfs 兜底
        Some((st, s)) if !s.is_empty() => (st, s),
        _ => read_sysfs_thermal_zones(), // 兜底：sysfs（shell cat，绕过 SELinux）
    };
    if sensors.is_empty() {
        return false;
    }
    let sensors_json: Vec<String> = sensors
        .iter()
        .map(|(name, type_, value)| format!("[\"{}\",{},{:.1}]", json_escape(name), type_, value))
        .collect();
    emit(&format!(
        "{{\"t\":\"temp\",\"ts\":{},\"status\":{},\"sensors\":[{}]}}",
        ts,
        status.unwrap_or(-1),
        sensors_json.join(",")
    ));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_thermalservice() {
        // 真机格式：Cached 区块在前，HAL 区块在后（后者覆盖前者）
        let out = "IsStatusOverride: false\n\
                   Thermal Status: 1\n\
                   Cached temperatures:\n\
                   \tTemperature{mValue=30.8, mType=3, mName=test temperature sensor, mStatus=0}\n\
                   HAL Ready: true\n\
                   Current temperatures from HAL:\n\
                   \tTemperature{mValue=42.5, mType=0, mName=soc0, mStatus=1}\n\
                   \tTemperature{mValue=41.0, mType=3, mName=skin, mStatus=0}\n\
                   Current cooling devices from HAL:\n\
                   \tCoolingDevice{mValue=100, mType=0, mName=test cooling device}\n\
                   Temperature static thresholds from HAL:\n\
                   \t{.type = SKIN}\n";
        let (status, sensors) = parse_thermalservice(out);
        assert_eq!(status, Some(1));
        assert_eq!(sensors.len(), 2);
        assert_eq!(sensors[0], ("soc0".to_string(), 0, 42.5));
        assert_eq!(sensors[1], ("skin".to_string(), 3, 41.0));
    }

    #[test]
    fn test_parse_thermalservice_empty() {
        assert_eq!(parse_thermalservice("garbage\n"), (None, Vec::new()));
    }
}

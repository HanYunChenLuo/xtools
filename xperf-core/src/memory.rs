use crate::utils;
use anyhow::Result;
use chrono::{DateTime, Local};
use serde::Serialize;
use std::collections::VecDeque;

// 定义内存详细类别结构
#[derive(Debug, Clone, Default, Serialize)]
pub struct MemoryDetails {
    pub java_heap: u64,
    pub native_heap: u64,
    pub code: u64,
    pub stack: u64,
    pub graphics: u64,
    pub private_other: u64,
    pub system: u64,
    pub total_pss: u64,
}

#[derive(Debug, Clone, Default)]
pub struct MemoryTimeSeriesData {
    pub timestamps: VecDeque<DateTime<Local>>,
    pub memory_details: VecDeque<MemoryDetails>,
}

impl MemoryTimeSeriesData {
    pub fn add_data_point(&mut self, timestamp: DateTime<Local>, details: MemoryDetails) {
        if self.timestamps.len() >= 2 * crate::CHART_SERIES_CAP {
            crate::decimate(&mut self.timestamps);
            crate::decimate(&mut self.memory_details);
        }
        self.timestamps.push_back(timestamp);
        self.memory_details.push_back(details);
    }
}

pub async fn sample_memory(pid: &str) -> Result<(u64, DateTime<Local>, MemoryDetails)> {
    let timestamp = Local::now();
    let output = utils::run_adb_command(&["shell", "dumpsys", "meminfo", pid])?.stdout;

    let mut total_pss = 0;
    let mut memory_details = MemoryDetails::default();
    let mut in_app_summary = false;
    let mut header_passed = false; // 用于跳过标题行

    // 解析App Summary部分
    for line in output.lines() {
        let line = line.trim();

        // 检测App Summary部分开始
        if line.contains("App Summary") {
            in_app_summary = true;
            continue;
        }

        // 跳过PSS/RSS标题行
        if in_app_summary && (line.contains("Pss(KB)") || line.contains("------")) {
            header_passed = line.contains("------");
            continue;
        }

        // 如果已经过了App Summary部分，则退出解析
        if in_app_summary && line.is_empty() {
            in_app_summary = false;
            continue;
        }

        // 解析App Summary部分的内存信息
        if in_app_summary && header_passed {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() >= 2 {
                let category = parts[0].trim();
                let values: Vec<&str> = parts[1].split_whitespace().collect();

                if !values.is_empty() {
                    if let Ok(kb) = values[0].parse::<u64>() {
                        match category {
                            "Java Heap" => memory_details.java_heap = kb,
                            "Native Heap" => memory_details.native_heap = kb,
                            "Code" => memory_details.code = kb,
                            "Stack" => memory_details.stack = kb,
                            "Graphics" => memory_details.graphics = kb,
                            "Private Other" => memory_details.private_other = kb,
                            "System" => memory_details.system = kb,
                            "TOTAL" | "TOTAL PSS" => {
                                memory_details.total_pss = kb;
                                total_pss = kb;
                            }
                            _ => {} // 忽略其他类别
                        }
                    }
                }
            }
        }

        // 如果不在App Summary中，仍然需要查找TOTAL PSS作为备用
        if !in_app_summary && line.starts_with("TOTAL PSS:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                if let Ok(kb) = parts[2].parse::<u64>() {
                    total_pss = kb;
                    if memory_details.total_pss == 0 {
                        memory_details.total_pss = kb;
                    }
                }
            }
        }
    }

    Ok((total_pss, timestamp, memory_details))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 模拟 `adb shell dumpsys meminfo <pid>` 的 App Summary 输出
    fn meminfo_output() -> &'static str {
        "\
TOTAL PSS:    453901
TOTAL SWAP PSS:      0

-------- App Summary --------
                           Pss(KB)
                           ------
        Java Heap:     7268
       Native Heap:   116908
              Code:    37100
             Stack:      512
          Graphics:        0
     Private Other:    12000
            System:   280113

           TOTAL PSS:   453901
"
    }

    fn meminfo_runner(args: &[&str]) -> anyhow::Result<crate::utils::ProcOutput> {
        // dumpsys meminfo <pid>
        if args.len() >= 4 && args[1] == "dumpsys" && args[2] == "meminfo" {
            return Ok(crate::utils::ProcOutput {
                stdout: meminfo_output().to_string(),
            });
        }
        Ok(crate::utils::ProcOutput { stdout: String::new() })
    }

    #[tokio::test]
    async fn test_sample_memory_parses_app_summary() {
        let _lock = crate::utils::ADB_TEST_LOCK.lock().await;
        crate::utils::set_adb_runner_for_test(meminfo_runner);
        let (total_pss, _ts, details) = sample_memory("15803").await.unwrap();
        assert_eq!(total_pss, 453901);
        assert_eq!(details.java_heap, 7268);
        assert_eq!(details.native_heap, 116908);
        assert_eq!(details.code, 37100);
        assert_eq!(details.stack, 512);
        assert_eq!(details.graphics, 0);
        assert_eq!(details.private_other, 12000);
        assert_eq!(details.system, 280113);
        assert_eq!(details.total_pss, 453901);
        crate::utils::clear_adb_runner_for_test();
    }

    /// 进程不存在时 dumpsys 输出空 → total_pss 保持 0
    fn empty_runner(_args: &[&str]) -> anyhow::Result<crate::utils::ProcOutput> {
        Ok(crate::utils::ProcOutput { stdout: String::new() })
    }

    #[tokio::test]
    async fn test_sample_memory_empty_output_returns_zero() {
        let _lock = crate::utils::ADB_TEST_LOCK.lock().await;
        crate::utils::set_adb_runner_for_test(empty_runner);
        let (total_pss, _ts, _details) = sample_memory("99999").await.unwrap();
        assert_eq!(total_pss, 0); // 无 TOTAL PSS 行 → 0
        crate::utils::clear_adb_runner_for_test();
    }
}

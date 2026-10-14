use std::fmt;

#[derive(Debug, Clone)]
pub struct SystemInfo {
    pub os: String,
    pub arch: String,
    pub cpu_brand: String,
    pub cpu_cores: usize,
    pub memory_gb: f64,
    pub lean_ctx_version: String,
    pub rust_version: String,
}

impl fmt::Display for SystemInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} | {} ({} cores) | {:.1} GB RAM",
            self.os, self.arch, self.cpu_brand, self.cpu_cores, self.memory_gb
        )
    }
}

pub fn collect() -> SystemInfo {
    SystemInfo {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        cpu_brand: read_cpu_brand(),
        cpu_cores: std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
        memory_gb: read_memory_gb(),
        lean_ctx_version: env!("CARGO_PKG_VERSION").to_string(),
        rust_version: read_rust_version(),
    }
}

fn read_cpu_brand() -> String {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map_or_else(|| "Unknown CPU".to_string(), |s| s.trim().to_string())
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("model name"))
                    .and_then(|l| l.split(':').nth(1))
                    .map(|v| v.trim().to_string())
            })
            .unwrap_or_else(|| "Unknown CPU".to_string())
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "(Get-CimInstance Win32_Processor).Name",
            ])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Unknown CPU".to_string())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        "Unknown CPU".to_string()
    }
}

fn read_memory_gb() -> f64 {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map_or(0.0, |bytes| bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|s| {
                s.lines().find(|l| l.starts_with("MemTotal")).and_then(|l| {
                    l.split_whitespace()
                        .nth(1)
                        .and_then(|v| v.parse::<u64>().ok())
                })
            })
            .map_or(0.0, |kb| kb as f64 / (1024.0 * 1024.0))
    }
    #[cfg(target_os = "windows")]
    {
        // Query total physical memory (bytes) via CIM; reliable on all
        // supported Windows versions and present on CI runners.
        std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory",
            ])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map_or(0.0, |bytes| bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        0.0
    }
}

fn read_rust_version() -> String {
    std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(|| "unknown".to_string(), |s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_returns_valid_info() {
        let info = collect();
        assert!(!info.os.is_empty(), "`info.os` is empty");
        assert!(!info.arch.is_empty(), "`info.arch` is empty");
        assert!(
            !info.lean_ctx_version.is_empty(),
            "`info.lean_ctx_version` is empty"
        );
        assert!(info.cpu_cores >= 1);
    }

    #[test]
    fn display_is_readable() {
        let info = collect();
        let s = format!("{info}");
        assert!(s.contains(&info.os));
        assert!(s.contains("cores"));
    }
}

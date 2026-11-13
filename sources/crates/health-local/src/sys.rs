//! Hardware detection: RAM, CPU, GPUs (with real VRAM).

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GpuInfo {
    pub name: String,
    pub vendor: GpuVendor,
    pub vram_gb: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SystemInfo {
    pub total_ram_gb: f64,
    pub available_ram_gb: f64,
    pub cpu_name: String,
    pub cpu_threads: usize,
    pub gpus: Vec<GpuInfo>,
}

impl SystemInfo {
    /// Largest dedicated VRAM among GPUs usable for offload (GB).
    pub fn best_vram_gb(&self) -> f64 {
        self.gpus.iter().filter(|g| g.vendor != GpuVendor::Other).filter_map(|g| g.vram_gb).fold(0.0, f64::max)
    }
    pub fn has_nvidia(&self) -> bool {
        self.gpus.iter().any(|g| g.vendor == GpuVendor::Nvidia)
    }
    pub fn has_gpu(&self) -> bool {
        self.gpus.iter().any(|g| g.vendor != GpuVendor::Other)
    }
    pub fn main_gpu(&self) -> Option<&GpuInfo> {
        self.gpus.iter().filter(|g| g.vendor != GpuVendor::Other).max_by(|a, b| a.vram_gb.unwrap_or(0.0).total_cmp(&b.vram_gb.unwrap_or(0.0)))
    }
}

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

#[cfg(windows)]
fn vendor_of(name: &str) -> GpuVendor {
    let n = name.to_lowercase();
    if n.contains("nvidia") || n.contains("geforce") || n.contains("quadro") || n.contains("rtx") {
        GpuVendor::Nvidia
    } else if n.contains("amd") || n.contains("radeon") {
        GpuVendor::Amd
    } else if n.contains("intel") || n.contains("arc") {
        GpuVendor::Intel
    } else {
        GpuVendor::Other
    }
}

/// Blocking detection (call via spawn_blocking).
pub fn detect() -> SystemInfo {
    let mut s = SystemInfo { cpu_threads: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4), ..Default::default() };
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        unsafe {
            let mut m: MEMORYSTATUSEX = std::mem::zeroed();
            m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
            if GlobalMemoryStatusEx(&mut m) != 0 {
                s.total_ram_gb = m.ullTotalPhys as f64 / GIB;
                s.available_ram_gb = m.ullAvailPhys as f64 / GIB;
            }
        }
        use winreg::enums::HKEY_LOCAL_MACHINE;
        let hklm = winreg::RegKey::predef(HKEY_LOCAL_MACHINE);
        if let Ok(k) = hklm.open_subkey(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0") {
            s.cpu_name = k.get_value::<String, _>("ProcessorNameString").map(|v| v.trim().to_string()).unwrap_or_default();
        }
        if let Ok(class) = hklm.open_subkey(r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}") {
            for sub in class.enum_keys().flatten() {
                if sub.len() != 4 || !sub.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                let Ok(k) = class.open_subkey(&sub) else { continue };
                let name: String = k.get_value("DriverDesc").unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                let vram = k
                    .get_raw_value("HardwareInformation.qwMemorySize")
                    .ok()
                    .and_then(|v| v.bytes.get(..8).map(|b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])))
                    .or_else(|| {
                        k.get_raw_value("HardwareInformation.MemorySize")
                            .ok()
                            .and_then(|v| v.bytes.get(..4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64))
                    })
                    .filter(|v| *v > 64 * 1024 * 1024)
                    .map(|v| v as f64 / GIB);
                let vendor = vendor_of(&name);
                if vendor == GpuVendor::Other && vram.is_none() {
                    continue;
                }
                if !s.gpus.iter().any(|g| g.name == name) {
                    s.gpus.push(GpuInfo { name, vendor, vram_gb: vram });
                }
            }
        }
    }
    #[cfg(target_os = "linux")]
    linux_detect(&mut s);
    // nvidia-smi gives exact VRAM for NVIDIA cards
    if let Some(out) = nvidia_smi() {
        for line in out.lines() {
            let mut parts = line.split(',').map(|p| p.trim());
            let (Some(name), Some(mem)) = (parts.next(), parts.next()) else { continue };
            let Ok(mib) = mem.parse::<f64>() else { continue };
            let vram = mib / 1024.0;
            let found = s.gpus.iter_mut().find(|g| {
                g.vendor == GpuVendor::Nvidia && (g.name.contains(name) || name.contains(&g.name) || (g.name.starts_with("NVIDIA GPU (") && g.vram_gb.is_none()))
            });
            match found {
                Some(g) => {
                    g.name = name.to_string();
                    g.vram_gb = Some(vram)
                }
                None => s.gpus.push(GpuInfo { name: name.to_string(), vendor: GpuVendor::Nvidia, vram_gb: Some(vram) }),
            }
        }
    }
    s
}

/// RAM from /proc/meminfo, CPU from /proc/cpuinfo, GPUs from /sys/class/drm
/// (AMD exposes VRAM in `mem_info_vram_total`; NVIDIA VRAM comes from nvidia-smi below).
#[cfg(target_os = "linux")]
fn linux_detect(s: &mut SystemInfo) {
    if let Ok(mi) = std::fs::read_to_string("/proc/meminfo") {
        let kb = |key: &str| {
            mi.lines()
                .find(|l| l.starts_with(key))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<f64>().ok())
                .map(|v| v * 1024.0 / GIB)
        };
        s.total_ram_gb = kb("MemTotal:").unwrap_or(0.0);
        s.available_ram_gb = kb("MemAvailable:").or_else(|| kb("MemFree:")).unwrap_or(0.0);
    }
    if let Ok(ci) = std::fs::read_to_string("/proc/cpuinfo") {
        if let Some(n) = ci.lines().find(|l| l.starts_with("model name")).and_then(|l| l.split_once(':')) {
            s.cpu_name = n.1.trim().to_string();
        }
    }
    let Ok(dir) = std::fs::read_dir("/sys/class/drm") else { return };
    let mut cards: Vec<_> = dir
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with("card") && n[4..].chars().all(|c| c.is_ascii_digit()))
        .collect();
    cards.sort();
    for card in cards {
        let dev = std::path::Path::new("/sys/class/drm").join(&card).join("device");
        let read = |f: &str| std::fs::read_to_string(dev.join(f)).map(|v| v.trim().to_string()).unwrap_or_default();
        let vendor = match read("vendor").as_str() {
            "0x10de" => GpuVendor::Nvidia,
            "0x1002" => GpuVendor::Amd,
            "0x8086" => GpuVendor::Intel,
            _ => continue,
        };
        let vram = read("mem_info_vram_total").parse::<f64>().ok().filter(|v| *v > 64.0 * 1024.0 * 1024.0).map(|v| v / GIB);
        let base = match vendor {
            GpuVendor::Nvidia => "NVIDIA GPU",
            GpuVendor::Amd => "AMD Radeon GPU",
            GpuVendor::Intel => "Intel GPU",
            GpuVendor::Other => "GPU",
        };
        let name = format!("{base} ({})", read("device"));
        s.gpus.push(GpuInfo { name, vendor, vram_gb: vram });
    }
}

fn nvidia_smi() -> Option<String> {
    let mut c = std::process::Command::new("nvidia-smi");
    c.args(["--query-gpu=name,memory.total", "--format=csv,noheader,nounits"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let out = c.output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn detects_something() {
        let s = super::detect();
        assert!(s.total_ram_gb > 0.5);
        assert!(s.cpu_threads >= 1);
    }
}

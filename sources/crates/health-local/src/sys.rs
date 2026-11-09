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
    // nvidia-smi gives exact VRAM for NVIDIA cards
    if let Some(out) = nvidia_smi() {
        for line in out.lines() {
            let mut parts = line.split(',').map(|p| p.trim());
            let (Some(name), Some(mem)) = (parts.next(), parts.next()) else { continue };
            let Ok(mib) = mem.parse::<f64>() else { continue };
            let vram = mib / 1024.0;
            match s.gpus.iter_mut().find(|g| g.vendor == GpuVendor::Nvidia && (g.name.contains(name) || name.contains(&g.name))) {
                Some(g) => g.vram_gb = Some(vram),
                None => s.gpus.push(GpuInfo { name: name.to_string(), vendor: GpuVendor::Nvidia, vram_gb: Some(vram) }),
            }
        }
    }
    s
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

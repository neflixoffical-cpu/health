//! Curated, verified catalog (catalog.json, built from the live Hugging Face API) and memory
//! estimates.

use crate::sys::SystemInfo;
use health_core::{t, Lang};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RemoteFile {
    pub path: String,
    pub size: u64,
    #[serde(default)]
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct KvProfile {
    /// KV-cache bytes per token for full-attention layers.
    #[serde(default)]
    pub per_token: u64,
    /// KV bytes per token for sliding-window layers (bounded by `swa_window`).
    #[serde(default)]
    pub swa_per_token: u64,
    #[serde(default)]
    pub swa_window: u64,
    /// Fixed state bytes (recurrent / hybrid layers).
    #[serde(default)]
    pub fixed: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    pub family: String,
    pub params: String,
    pub description_ru: String,
    pub description_en: String,
    pub tags: Vec<String>,
    pub repo: String,
    #[serde(default)]
    pub revision: Option<String>,
    pub file: String,
    pub parts: u32,
    pub size_bytes: u64,
    pub quant: String,
    #[serde(default)]
    pub context: Option<u64>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub architecture: Option<String>,
    #[serde(default)]
    pub layers: Option<u64>,
    #[serde(default)]
    pub kv: KvProfile,
    pub files: Vec<RemoteFile>,
    #[serde(default)]
    pub mmproj: Option<RemoteFile>,
}

impl CatalogEntry {
    pub fn description(&self) -> &str {
        match health_core::lang() {
            Lang::Ru => &self.description_ru,
            Lang::En => &self.description_en,
        }
    }
    pub fn has_tag(&self, t: &str) -> bool {
        self.tags.iter().any(|x| x.eq_ignore_ascii_case(t))
    }
    pub fn total_download(&self) -> u64 {
        self.size_bytes + self.mmproj.as_ref().map(|m| m.size).unwrap_or(0)
    }
    /// Estimated memory need (GB) for the given context length.
    pub fn ram_gb(&self, ctx: u32) -> f64 {
        let ctx = ctx as u64;
        let kv = if self.kv.per_token > 0 || self.kv.swa_per_token > 0 || self.kv.fixed > 0 {
            self.kv.per_token * ctx + self.kv.swa_per_token * ctx.min(self.kv.swa_window.max(1)) + self.kv.fixed
        } else {
            estimate_kv_bytes(self.size_bytes, ctx)
        };
        estimate_ram_gb(self.size_bytes + self.mmproj.as_ref().map(|m| m.size).unwrap_or(0), kv)
    }
}

#[derive(Deserialize)]
struct CatalogDoc {
    models: Vec<CatalogEntry>,
}

/// The verified catalog (sorted by size).
pub fn catalog() -> &'static [CatalogEntry] {
    static CAT: OnceLock<Vec<CatalogEntry>> = OnceLock::new();
    CAT.get_or_init(|| {
        let mut v = serde_json::from_str::<CatalogDoc>(include_str!("catalog.json")).map(|d| d.models).unwrap_or_default();
        v.sort_by_key(|e| e.size_bytes);
        v
    })
}

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// Rough KV-cache size when the architecture is unknown (scaled from a Llama-3-8B-like model).
pub fn estimate_kv_bytes(weights_bytes: u64, ctx: u64) -> u64 {
    let size_gb = weights_bytes as f64 / GIB;
    let per_token = 131_072.0 * (size_gb / 4.6).max(0.15).sqrt();
    (per_token * ctx as f64) as u64
}

/// Memory needed to run: weights + KV cache + runtime overhead (GB).
pub fn estimate_ram_gb(weights_bytes: u64, kv_bytes: u64) -> f64 {
    (weights_bytes as f64 * 1.03 + kv_bytes as f64) / GIB + 0.6
}

/// Convenience used by the UI for arbitrary files.
pub fn estimate_for(size_bytes: u64, ctx: u32) -> f64 {
    estimate_ram_gb(size_bytes, estimate_kv_bytes(size_bytes, ctx as u64))
}

/// Will a model of `need_gb` run well on this machine?
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    /// Fits entirely in video memory — fastest.
    Gpu,
    /// Fits in RAM (GPU helps with part of the layers).
    Ram,
    /// Needs most of the memory — will be slow / may swap.
    Tight,
    /// Not enough memory.
    TooBig,
}

impl Fit {
    pub fn label(self) -> &'static str {
        match self {
            Fit::Gpu => t!("в видеопамять", "fits in VRAM"),
            Fit::Ram => t!("в ОЗУ", "fits in RAM"),
            Fit::Tight => t!("впритык", "tight"),
            Fit::TooBig => t!("не хватит памяти", "too big"),
        }
    }
    pub fn glyph(self) -> &'static str {
        match self {
            Fit::Gpu => "●",
            Fit::Ram => "●",
            Fit::Tight => "◐",
            Fit::TooBig => "○",
        }
    }
}

pub fn fit(sys: &SystemInfo, need_gb: f64) -> Fit {
    let vram = sys.best_vram_gb();
    let ram = if sys.total_ram_gb > 0.0 { sys.total_ram_gb } else { 8.0 };
    if vram > 0.0 && need_gb <= vram * 0.92 {
        Fit::Gpu
    } else if need_gb <= ram * 0.70 + vram * 0.85 {
        Fit::Ram
    } else if need_gb <= ram * 0.92 + vram * 0.92 {
        Fit::Tight
    } else {
        Fit::TooBig
    }
}

/// RAM tiers used to group the catalog (GB).
pub const TIERS: [u32; 10] = [2, 4, 8, 12, 16, 24, 32, 48, 64, 80];

/// Smallest tier that holds `gb`.
pub fn tier_of(gb: f64) -> u32 {
    TIERS.iter().copied().find(|t| gb <= *t as f64).unwrap_or(128)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::{GpuInfo, GpuVendor};

    #[test]
    fn catalog_spans_2_to_80_gb() {
        let c = catalog();
        assert!(c.len() >= 25, "catalog has {} entries", c.len());
        let needs: Vec<f64> = c.iter().map(|e| e.ram_gb(8192)).collect();
        assert!(needs.iter().cloned().fold(f64::MAX, f64::min) <= 2.0, "smallest {:?}", needs.first());
        let max = needs.iter().cloned().fold(0.0, f64::max);
        assert!(max > 60.0 && max <= 90.0, "largest {max}");
        for e in c {
            assert!(!e.files.is_empty() && e.files.iter().all(|f| f.size > 0), "{}", e.id);
            assert_eq!(e.files.len() as u32, e.parts, "{}", e.id);
            assert!(!e.description_ru.is_empty() && !e.description_en.is_empty());
        }
    }

    #[test]
    fn fits() {
        let sys = SystemInfo { total_ram_gb: 32.0, gpus: vec![GpuInfo { name: "RTX".into(), vendor: GpuVendor::Nvidia, vram_gb: Some(16.0) }], ..Default::default() };
        assert_eq!(fit(&sys, 8.0), Fit::Gpu);
        assert_eq!(fit(&sys, 30.0), Fit::Ram);
        assert_eq!(fit(&sys, 40.0), Fit::Tight);
        assert_eq!(fit(&sys, 80.0), Fit::TooBig);
        assert_eq!(tier_of(1.4), 2);
        assert_eq!(tier_of(70.0), 80);
    }
}

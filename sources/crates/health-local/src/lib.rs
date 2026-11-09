//! health-local — run open models on this PC.
//!
//! * [`LocalManager`] — façade used by the `/localmodel` panel and the backend resolver.
//! * Verified catalog (2–80 GB RAM tiers), Hugging Face search, resumable downloads with
//!   SHA-256, GGUF metadata, llama.cpp runtime install/start (kill-on-close job), Ollama listing.

pub mod catalog;
pub mod download;
pub mod gguf;
pub mod hf;
pub mod runtime;
pub mod sys;

pub use catalog::{catalog, estimate_for, fit, tier_of, CatalogEntry, Fit, RemoteFile, TIERS};
pub use download::{DlState, DownloadId, DownloadRequest, DownloadState};
pub use gguf::{read_meta, GgufMeta};
pub use hf::{GgufVariant, HfModel};
pub use runtime::{InstalledRuntime, RunningServer, RuntimeStatus};
pub use sys::{GpuInfo, GpuVendor, SystemInfo};

use anyhow::{anyhow, bail, Result};
use health_core::{paths, t, LocalBackend, ModelInfo, ModelOrigin, ProviderKind, SharedConfig};
use parking_lot::RwLock;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// A downloaded model on disk.
#[derive(Clone, Debug, PartialEq)]
pub struct InstalledModel {
    /// First (or only) GGUF file.
    pub path: PathBuf,
    pub name: String,
    pub size_bytes: u64,
    pub quant: String,
    pub repo: Option<String>,
    pub parts: u32,
    pub mmproj: Option<PathBuf>,
    /// Matching catalog entry id, if any.
    pub catalog_id: Option<String>,
}

pub struct LocalManager {
    config: SharedConfig,
    sys: RwLock<SystemInfo>,
    downloads: download::Downloads,
    runtime: runtime::Runtime,
}

impl LocalManager {
    pub fn new(config: SharedConfig) -> Arc<LocalManager> {
        let (tag, backend) = {
            let c = config.read();
            (c.local.runtime_tag.clone(), c.local.runtime_backend)
        };
        let rt = runtime::Runtime::new();
        rt.set_installed(runtime::Runtime::installed(tag.as_deref(), backend));
        let m = Arc::new(LocalManager { config, sys: RwLock::new(SystemInfo::default()), downloads: download::Downloads::new(), runtime: rt });
        // hardware detection takes a moment (registry + nvidia-smi) — do it off-thread
        let weak = Arc::downgrade(&m);
        std::thread::spawn(move || {
            let s = sys::detect();
            if let Some(m) = weak.upgrade() {
                *m.sys.write() = s;
            }
        });
        m
    }

    pub fn system(&self) -> SystemInfo {
        self.sys.read().clone()
    }

    pub async fn refresh_system(&self) {
        if let Ok(s) = tokio::task::spawn_blocking(sys::detect).await {
            *self.sys.write() = s;
        }
    }

    pub fn catalog(&self) -> &'static [CatalogEntry] {
        catalog::catalog()
    }

    pub fn fit(&self, need_gb: f64) -> Fit {
        catalog::fit(&self.sys.read(), need_gb)
    }

    pub fn context_size(&self) -> u32 {
        self.config.read().local.context_size
    }

    pub fn models_dir(&self) -> PathBuf {
        self.config.read().local.models_dir()
    }

    /// Change the download folder (created if needed). Existing models are not moved.
    pub fn set_models_dir(&self, input: &str) -> Result<PathBuf> {
        let p = paths::expand_user_path(input);
        if !p.is_absolute() {
            bail!(t!("Укажите полный путь, например D:\\AI\\models", "Use a full path, e.g. D:\\AI\\models"));
        }
        std::fs::create_dir_all(&p).map_err(|e| anyhow!("{} {}: {e}", t!("Не удалось создать папку", "Cannot create folder"), p.display()))?;
        let probe = p.join(".health-write-test");
        std::fs::write(&probe, b"ok").map_err(|e| anyhow!("{} {}: {e}", t!("Нет прав на запись в", "No write access to"), p.display()))?;
        let _ = std::fs::remove_file(probe);
        let snapshot = {
            let mut c = self.config.write();
            c.local.models_dir = Some(p.display().to_string());
            c.clone()
        };
        snapshot.save()?;
        Ok(p)
    }

    /// Free space on the models drive.
    pub fn free_space(&self) -> Option<u64> {
        download::free_space(&self.models_dir())
    }

    // ── Hugging Face ──
    pub async fn hf_search(&self, query: &str, limit: usize) -> Result<Vec<HfModel>> {
        hf::search(query, limit).await
    }
    pub async fn hf_files(&self, repo: &str) -> Result<Vec<GgufVariant>> {
        hf::files(repo).await
    }

    // ── downloads ──
    pub fn download_catalog(&self, e: &CatalogEntry) -> DownloadId {
        self.downloads.enqueue(DownloadRequest {
            repo: e.repo.clone(),
            files: e.files.clone(),
            mmproj: e.mmproj.clone(),
            label: format!("{} · {}", e.name, e.quant),
            dest_dir: self.models_dir().join(download::repo_dir(&e.repo)),
            base_url: None,
        })
    }
    pub fn download_variant(&self, v: &GgufVariant) -> DownloadId {
        self.downloads.enqueue(DownloadRequest {
            repo: v.repo.clone(),
            files: v.files.clone(),
            mmproj: v.mmproj.clone(),
            label: format!("{} · {}", v.repo.rsplit('/').next().unwrap_or(&v.repo), v.quant),
            dest_dir: self.models_dir().join(download::repo_dir(&v.repo)),
            base_url: None,
        })
    }
    pub fn cancel_download(&self, id: DownloadId) {
        self.downloads.cancel(id)
    }
    pub fn downloads(&self) -> Vec<DownloadState> {
        self.downloads.list()
    }
    pub fn downloads_version(&self) -> tokio::sync::watch::Receiver<u64> {
        self.downloads.version()
    }
    pub fn clear_finished_downloads(&self) {
        self.downloads.clear_finished()
    }
    pub fn downloading(&self) -> bool {
        self.downloads.is_active()
    }

    // ── installed models ──
    pub fn installed(&self) -> Vec<InstalledModel> {
        scan_installed(&self.models_dir())
    }

    pub fn is_installed(&self, e: &CatalogEntry) -> bool {
        let first = self.models_dir().join(download::repo_dir(&e.repo)).join(download::local_name(&e.file));
        std::fs::metadata(&first).map(|m| m.len() == e.files[0].size).unwrap_or(false)
    }

    /// Delete a model (all split parts; the projector too if no other model uses the folder).
    pub fn delete_installed(&self, m: &InstalledModel) -> Result<()> {
        let dir = m.path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let name = m.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let prefix = gguf::split_parts(&name).map(|(p, _, _)| p);
        for e in std::fs::read_dir(&dir)?.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            let same = match &prefix {
                Some(p) => gguf::split_parts(&n).map(|(pp, _, _)| &pp == p).unwrap_or(false),
                None => n == name,
            };
            if same {
                std::fs::remove_file(e.path())?;
            }
        }
        let others = scan_installed(&dir).into_iter().filter(|x| x.path != m.path).count();
        if others == 0 {
            if let Some(mm) = &m.mmproj {
                let _ = std::fs::remove_file(mm);
            }
            let _ = std::fs::remove_dir(&dir);
        }
        Ok(())
    }

    /// Installed GGUFs as models for the picker.
    pub fn local_models(&self) -> Vec<ModelInfo> {
        let ctx = self.context_size() as u64;
        self.installed()
            .into_iter()
            .map(|m| ModelInfo {
                id: m.path.display().to_string(),
                name: Some(m.name.clone()),
                provider: ProviderKind::LocalLlama,
                base_url: None,
                origin: ModelOrigin::Local,
                context_window: Some(ctx),
                vision: Some(m.mmproj.is_some()),
                tools: Some(true),
                description: Some(format!("{} · {}", m.quant, health_core::util::human_bytes(m.size_bytes))),
                ..Default::default()
            })
            .collect()
    }

    // ── runtime ──
    pub fn runtime_status(&self) -> RuntimeStatus {
        self.runtime.status()
    }
    pub fn server_log_tail(&self) -> Vec<String> {
        self.runtime.log_tail()
    }

    fn concrete_backend(&self, b: LocalBackend) -> LocalBackend {
        match b {
            LocalBackend::Auto => {
                let s = self.sys.read();
                if s.has_nvidia() {
                    LocalBackend::Cuda
                } else if s.has_gpu() {
                    LocalBackend::Vulkan
                } else {
                    LocalBackend::Cpu
                }
            }
            other => other,
        }
    }

    fn prefer_new_cuda(&self) -> bool {
        // CUDA 13 dropped Maxwell/Pascal/Volta (GTX 9xx/10xx, Titan V)
        !self.sys.read().gpus.iter().any(|g| g.vendor == GpuVendor::Nvidia && (g.name.contains("GTX") || g.name.contains("TITAN V") || g.name.contains("Quadro P")))
    }

    /// Install (or reinstall) llama.cpp for a backend.
    pub async fn install_runtime(&self, backend: LocalBackend) -> Result<InstalledRuntime> {
        if self.sys.read().total_ram_gb == 0.0 {
            self.refresh_system().await;
        }
        let b = self.concrete_backend(backend);
        let rt = self.runtime.install(b, self.prefer_new_cuda(), &CancellationToken::new()).await?;
        let snapshot = {
            let mut c = self.config.write();
            c.local.runtime_tag = Some(rt.tag.clone());
            c.local.runtime_backend = Some(rt.backend);
            c.clone()
        };
        let _ = snapshot.save();
        Ok(rt)
    }

    /// Make sure llama-server runs `model_path`; returns the OpenAI-compatible base URL.
    pub async fn ensure_running(&self, model_path: &str, progress: &(dyn Fn(String) + Send + Sync)) -> Result<String> {
        if let Some(r) = self.runtime.running().await {
            if r.model_path == model_path && r.ready {
                return Ok(format!("http://127.0.0.1:{}/v1", r.port));
            }
        }
        let path = PathBuf::from(model_path);
        if !path.is_file() {
            bail!("{} {}", t!("Файл модели не найден:", "Model file not found:"), path.display());
        }
        let cfg = self.config.read().local.clone();
        let rt = match self.runtime.status().installed {
            Some(rt) if cfg.backend == LocalBackend::Auto || rt.backend == self.concrete_backend(cfg.backend) => rt,
            _ => {
                progress(t!("Устанавливаю движок llama.cpp (один раз)…", "Installing the llama.cpp engine (one time)…").into());
                self.install_runtime(cfg.backend).await?
            }
        };
        let size: u64 = scan_installed(path.parent().unwrap_or(Path::new(".")))
            .into_iter()
            .find(|m| m.path == path)
            .map(|m| m.size_bytes)
            .unwrap_or_else(|| std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0));
        let sys = self.system();
        let layers = match cfg.gpu_layers {
            Some(n) => n,
            None if rt.backend == LocalBackend::Cpu || !sys.has_gpu() => 0,
            None => {
                let vram = sys.best_vram_gb() * 1024.0 * 1024.0 * 1024.0;
                if (size as f64) < vram * 0.88 {
                    999
                } else {
                    let blocks = gguf::read_meta(&path).ok().and_then(|m| m.block_count).unwrap_or(40) as f64;
                    ((vram * 0.82 / size.max(1) as f64) * blocks).floor().max(0.0) as i32
                }
            }
        };
        let mmproj = scan_installed(path.parent().unwrap_or(Path::new("."))).into_iter().find(|m| m.path == path).and_then(|m| m.mmproj);
        progress(t!("Запускаю llama-server…", "Starting llama-server…").into());
        let port = self
            .runtime
            .start(&rt, &path, mmproj.as_deref(), cfg.context_size, layers, cfg.threads, cfg.port, &cfg.extra_args, progress)
            .await?;
        Ok(format!("http://127.0.0.1:{port}/v1"))
    }

    pub async fn stop_server(&self) {
        self.runtime.stop().await
    }

    // ── Ollama ──
    pub async fn ollama_models(&self) -> Vec<ModelInfo> {
        let (on, url) = {
            let c = self.config.read();
            (c.local.use_ollama, c.local.ollama_url.clone())
        };
        if !on {
            return Vec::new();
        }
        let http = hf::client();
        let Ok(resp) = http.get(format!("{}/api/tags", url.trim_end_matches('/'))).timeout(Duration::from_millis(1500)).send().await else {
            return Vec::new();
        };
        let Ok(v) = resp.json::<serde_json::Value>().await else { return Vec::new() };
        v.get("models")
            .and_then(|m| m.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|m| {
                        let name = m.get("name").or_else(|| m.get("model"))?.as_str()?.to_string();
                        let params = m.pointer("/details/parameter_size").and_then(|x| x.as_str()).unwrap_or("");
                        let quant = m.pointer("/details/quantization_level").and_then(|x| x.as_str()).unwrap_or("");
                        Some(ModelInfo {
                            id: name.clone(),
                            name: Some(name),
                            provider: ProviderKind::Ollama,
                            origin: ModelOrigin::Ollama,
                            description: Some(format!("{params} {quant}").trim().to_string()),
                            ..Default::default()
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Scan a models folder (recursively, 3 levels).
pub fn scan_installed(root: &Path) -> Vec<InstalledModel> {
    let mut out = Vec::new();
    scan_dir(root, 0, &mut out);
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

fn scan_dir(dir: &Path, depth: usize, out: &mut Vec<InstalledModel>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    let files: Vec<&PathBuf> = entries.iter().filter(|p| p.is_file()).collect();
    let mmproj = files.iter().find(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase().contains("mmproj")).unwrap_or(false)).map(|p| (*p).clone());
    let repo = dir.file_name().map(|n| n.to_string_lossy().to_string()).filter(|n| n.contains("__")).map(|n| n.replacen("__", "/", 1));
    for f in &files {
        let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if !name.to_lowercase().ends_with(".gguf") || gguf::is_aux(&name) {
            continue;
        }
        let (parts, size) = match gguf::split_parts(&name) {
            Some((prefix, 1, total)) => {
                let size: u64 = files
                    .iter()
                    .filter(|o| o.file_name().and_then(|n| gguf::split_parts(&n.to_string_lossy())).map(|(p, _, _)| p == prefix).unwrap_or(false))
                    .filter_map(|o| std::fs::metadata(o).ok().map(|m| m.len()))
                    .sum();
                (total, size)
            }
            Some(_) => continue,
            None => (1, std::fs::metadata(f).map(|m| m.len()).unwrap_or(0)),
        };
        let cat = catalog::catalog().iter().find(|e| download::local_name(&e.file).eq_ignore_ascii_case(&name));
        let quant = gguf::quant_of(&name);
        let pretty = match cat {
            Some(e) => format!("{} · {}", e.name, e.quant),
            None => {
                let stem = name.trim_end_matches(".gguf");
                let stem = gguf::split_parts(&name).map(|(p, _, _)| p).unwrap_or_else(|| stem.to_string());
                stem.rsplit('/').next().unwrap_or(&stem).to_string()
            }
        };
        out.push(InstalledModel { path: (*f).clone(), name: pretty, size_bytes: size, quant, repo: repo.clone(), parts, mmproj: mmproj.clone(), catalog_id: cat.map(|e| e.id.clone()) });
    }
    if depth < 3 {
        for d in entries.iter().filter(|p| p.is_dir()) {
            let n = d.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if n.starts_with('.') || n == "runtime" {
                continue;
            }
            scan_dir(d, depth + 1, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scans_split_and_mmproj() {
        let d = std::env::temp_dir().join(format!("health-scan-{}", health_core::util::short_id())).join("owner__repo");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("Big-Q4_K_M-00001-of-00002.gguf"), vec![0u8; 10]).unwrap();
        std::fs::write(d.join("Big-Q4_K_M-00002-of-00002.gguf"), vec![0u8; 15]).unwrap();
        std::fs::write(d.join("mmproj-F16.gguf"), vec![0u8; 3]).unwrap();
        std::fs::write(d.join("x.gguf.part"), vec![0u8; 3]).unwrap();
        let found = scan_installed(d.parent().unwrap());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].size_bytes, 25);
        assert_eq!(found[0].parts, 2);
        assert_eq!(found[0].repo.as_deref(), Some("owner/repo"));
        assert!(found[0].mmproj.is_some());
        let _ = std::fs::remove_dir_all(d.parent().unwrap());
    }
}

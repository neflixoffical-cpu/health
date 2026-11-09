//! llama.cpp runtime: install official Windows builds from GitHub and run `llama-server.exe`
//! inside a kill-on-close Job object (the server dies together with Health).

use crate::download::fetch_file;
use anyhow::{anyhow, bail, Context, Result};
use health_core::{hlog, paths, t, tf, LocalBackend};
use parking_lot::Mutex;
use serde_json::Value;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::sync::CancellationToken;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug, PartialEq)]
pub struct InstalledRuntime {
    pub tag: String,
    pub backend: LocalBackend,
    pub dir: PathBuf,
    pub server_exe: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RunningServer {
    pub model_path: String,
    pub port: u16,
    pub pid: u32,
    pub started_at: i64,
    pub ready: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RuntimeStatus {
    pub installed: Option<InstalledRuntime>,
    pub running: Option<RunningServer>,
    /// Human progress text while installing / starting.
    pub busy: Option<String>,
    /// 0..1 when known.
    pub progress: Option<f64>,
}

/// One asset to download.
#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

fn backend_slug(b: LocalBackend) -> &'static str {
    match b {
        LocalBackend::Cuda => "cuda",
        LocalBackend::Vulkan => "vulkan",
        LocalBackend::Cpu => "cpu",
        LocalBackend::Auto => "auto",
    }
}

/// Choose the newest release that has the Windows x64 build for `backend`
/// (`prefer_new_cuda` = pick the highest CUDA major, else the lowest).
pub fn pick_assets(releases: &Value, backend: LocalBackend, prefer_new_cuda: bool) -> Option<(String, Vec<Asset>)> {
    for r in releases.as_array()? {
        if r.get("draft").and_then(|d| d.as_bool()) == Some(true) {
            continue;
        }
        let tag = r.get("tag_name")?.as_str()?.to_string();
        let assets: Vec<Asset> = r
            .get("assets")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| {
                        Some(Asset {
                            name: x.get("name")?.as_str()?.to_string(),
                            url: x.get("browser_download_url")?.as_str()?.to_string(),
                            size: x.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let find = |suffix: &str| assets.iter().find(|a| a.name.starts_with("llama-") && a.name.ends_with(suffix)).cloned();
        match backend {
            LocalBackend::Cpu => {
                if let Some(a) = find("-bin-win-cpu-x64.zip") {
                    return Some((tag, vec![a]));
                }
            }
            LocalBackend::Vulkan => {
                if let Some(a) = find("-bin-win-vulkan-x64.zip") {
                    return Some((tag, vec![a]));
                }
            }
            LocalBackend::Cuda | LocalBackend::Auto => {
                let mut cuda: Vec<(f64, Asset)> = assets
                    .iter()
                    .filter(|a| a.name.starts_with("llama-") && a.name.contains("-bin-win-cuda-") && a.name.ends_with("-x64.zip"))
                    .filter_map(|a| {
                        let ver = a.name.split("-bin-win-cuda-").nth(1)?.trim_end_matches("-x64.zip").to_string();
                        Some((ver.parse::<f64>().ok()?, a.clone()))
                    })
                    .collect();
                cuda.sort_by(|a, b| a.0.total_cmp(&b.0));
                let chosen = if prefer_new_cuda { cuda.last() } else { cuda.first() };
                if let Some((ver, main)) = chosen {
                    let ver_s = main.name.split("-bin-win-cuda-").nth(1).unwrap_or("").trim_end_matches("-x64.zip").to_string();
                    let rt = assets.iter().find(|a| a.name == format!("cudart-llama-bin-win-cuda-{ver_s}-x64.zip")).cloned();
                    let _ = ver;
                    let mut v = vec![main.clone()];
                    if let Some(rt) = rt {
                        v.push(rt);
                    }
                    return Some((tag, v));
                }
            }
        }
    }
    None
}

fn find_exe(dir: &Path, name: &str) -> Option<PathBuf> {
    let direct = dir.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find_exe(&p, name) {
                return Some(f);
            }
        }
    }
    None
}

struct ServerProc {
    child: tokio::process::Child,
    job: usize,
    info: RunningServer,
}

impl Drop for ServerProc {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        close_job(self.job);
    }
}

fn close_job(job: usize) {
    #[cfg(windows)]
    if job != 0 {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(job as windows_sys::Win32::Foundation::HANDLE);
        }
    }
    #[cfg(not(windows))]
    let _ = job;
}

/// Put a process into a new kill-on-close job. Returns the job handle (0 on failure).
#[cfg(windows)]
fn job_for(process: std::os::windows::io::RawHandle) -> usize {
    use windows_sys::Win32::System::JobObjects::*;
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return 0;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if ok == 0 || AssignProcessToJobObject(job, process as _) == 0 {
            windows_sys::Win32::Foundation::CloseHandle(job);
            return 0;
        }
        job as usize
    }
}

pub struct Runtime {
    http: reqwest::Client,
    state: Mutex<RuntimeStatus>,
    server: tokio::sync::Mutex<Option<ServerProc>>,
    tail: Arc<Mutex<VecDeque<String>>>,
    install_lock: tokio::sync::Mutex<()>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl Runtime {
    pub fn new() -> Self {
        Runtime {
            http: crate::hf::client(),
            state: Mutex::new(RuntimeStatus::default()),
            server: tokio::sync::Mutex::new(None),
            tail: Arc::new(Mutex::new(VecDeque::new())),
            install_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn llama_root() -> PathBuf {
        paths::runtime_dir().join("llama.cpp")
    }

    /// Installed runtime according to the config (verified on disk).
    pub fn installed(tag: Option<&str>, backend: Option<LocalBackend>) -> Option<InstalledRuntime> {
        let (tag, backend) = (tag?, backend?);
        let dir = Self::llama_root().join(format!("{tag}-{}", backend_slug(backend)));
        let exe = find_exe(&dir, "llama-server.exe")?;
        Some(InstalledRuntime { tag: tag.to_string(), backend, dir, server_exe: exe })
    }

    pub fn status(&self) -> RuntimeStatus {
        self.state.lock().clone()
    }

    pub fn set_installed(&self, rt: Option<InstalledRuntime>) {
        self.state.lock().installed = rt;
    }

    fn busy(&self, text: Option<String>, progress: Option<f64>) {
        let mut s = self.state.lock();
        s.busy = text;
        s.progress = progress;
    }

    pub fn log_tail(&self) -> Vec<String> {
        self.tail.lock().iter().cloned().collect()
    }

    /// Download and unpack llama.cpp. `backend` must be concrete (not Auto).
    pub async fn install(&self, backend: LocalBackend, prefer_new_cuda: bool, cancel: &CancellationToken) -> Result<InstalledRuntime> {
        let _g = self.install_lock.lock().await;
        let r = self.install_inner(backend, prefer_new_cuda, cancel).await;
        self.busy(None, None);
        r
    }

    async fn install_inner(&self, backend: LocalBackend, prefer_new_cuda: bool, cancel: &CancellationToken) -> Result<InstalledRuntime> {
        self.busy(Some(t!("Ищу свежую сборку llama.cpp…", "Looking up the latest llama.cpp build…").into()), None);
        let resp = self
            .http
            .get("https://api.github.com/repos/ggml-org/llama.cpp/releases?per_page=20")
            .header("accept", "application/vnd.github+json")
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| anyhow!("{}: {e}", t!("Нет связи с GitHub", "Can't reach GitHub")))?;
        let status = resp.status().as_u16();
        if status == 403 || status == 429 {
            bail!(t!("GitHub временно ограничил запросы — попробуйте через несколько минут", "GitHub rate limit reached — try again in a few minutes"));
        }
        if !(200..300).contains(&status) {
            bail!("GitHub: HTTP {status}");
        }
        let releases: Value = resp.json().await?;
        let (tag, assets) = pick_assets(&releases, backend, prefer_new_cuda)
            .ok_or_else(|| anyhow!(t!("Не нашёл подходящую сборку llama.cpp для Windows", "No suitable llama.cpp Windows build found")))?;
        let target = Self::llama_root().join(format!("{tag}-{}", backend_slug(backend)));
        let dl_dir = paths::runtime_dir().join("_downloads");
        std::fs::create_dir_all(&dl_dir)?;
        let total: u64 = assets.iter().map(|a| a.size).sum::<u64>().max(1);
        let mut done_before = 0u64;
        for a in &assets {
            let dest = dl_dir.join(&a.name);
            let label = tf!("Скачиваю llama.cpp {} ({})", "Downloading llama.cpp {} ({})", tag, a.name);
            let state = &self.state;
            let base = done_before;
            fetch_file(&self.http, &a.url, &dest, a.size, None, cancel, |bytes, _| {
                let mut s = state.lock();
                s.busy = Some(label.clone());
                s.progress = Some((base + bytes) as f64 / total as f64);
            })
            .await
            .with_context(|| a.name.clone())?;
            done_before += a.size;
        }
        self.busy(Some(t!("Распаковываю…", "Unpacking…").into()), Some(1.0));
        let t2 = target.clone();
        let zips: Vec<PathBuf> = assets.iter().map(|a| dl_dir.join(&a.name)).collect();
        tokio::task::spawn_blocking(move || -> Result<()> {
            if t2.exists() {
                std::fs::remove_dir_all(&t2)?;
            }
            std::fs::create_dir_all(&t2)?;
            for z in &zips {
                let f = std::fs::File::open(z)?;
                let mut ar = zip::ZipArchive::new(f).context("zip")?;
                ar.extract(&t2).context("extract")?;
            }
            Ok(())
        })
        .await??;
        let exe = find_exe(&target, "llama-server.exe").ok_or_else(|| anyhow!(t!("В архиве нет llama-server.exe", "llama-server.exe not found in the archive")))?;
        // cudart DLLs must sit next to the server
        if let Some(server_dir) = exe.parent() {
            if server_dir != target {
                for e in std::fs::read_dir(&target)?.flatten() {
                    let p = e.path();
                    if p.extension().map(|x| x.eq_ignore_ascii_case("dll")).unwrap_or(false) {
                        let _ = std::fs::copy(&p, server_dir.join(e.file_name()));
                    }
                }
            }
        }
        for a in &assets {
            let _ = std::fs::remove_file(dl_dir.join(&a.name));
        }
        // remove older runtimes
        if let Ok(rd) = std::fs::read_dir(Self::llama_root()) {
            for e in rd.flatten() {
                if e.path() != target && e.path().is_dir() {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
        let rt = InstalledRuntime { tag, backend, dir: target, server_exe: exe };
        self.state.lock().installed = Some(rt.clone());
        Ok(rt)
    }

    pub async fn running(&self) -> Option<RunningServer> {
        let mut g = self.server.lock().await;
        if let Some(p) = g.as_mut() {
            if let Ok(Some(_)) = p.child.try_wait() {
                *g = None;
                self.state.lock().running = None;
                return None;
            }
            return Some(p.info.clone());
        }
        None
    }

    pub async fn stop(&self) {
        let p = self.server.lock().await.take();
        if let Some(mut p) = p {
            let _ = p.child.kill().await;
        }
        self.state.lock().running = None;
    }

    /// Start `llama-server` for a model and wait until it is ready. Returns the port.
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        &self,
        rt: &InstalledRuntime,
        model: &Path,
        mmproj: Option<&Path>,
        ctx: u32,
        gpu_layers: i32,
        threads: Option<u32>,
        port: Option<u16>,
        extra: &[String],
        progress: &(dyn Fn(String) + Send + Sync),
    ) -> Result<u16> {
        self.stop().await;
        let port = match port {
            Some(p) => p,
            None => {
                let l = std::net::TcpListener::bind("127.0.0.1:0")?;
                l.local_addr()?.port()
            }
        };
        let mut c = std::process::Command::new(&rt.server_exe);
        c.arg("-m").arg(model).args(["--host", "127.0.0.1", "--port", &port.to_string(), "-c", &ctx.to_string(), "-ngl", &gpu_layers.to_string(), "--jinja"]);
        if let Some(mm) = mmproj {
            c.arg("--mmproj").arg(mm);
        }
        if let Some(t) = threads {
            c.args(["-t", &t.to_string()]);
        }
        c.args(extra);
        if let Some(dir) = rt.server_exe.parent() {
            c.current_dir(dir);
        }
        c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = tokio::process::Command::from(c).kill_on_drop(true).spawn().with_context(|| format!("start {}", rt.server_exe.display()))?;
        #[cfg(windows)]
        let job = child.raw_handle().map(job_for).unwrap_or(0);
        #[cfg(not(windows))]
        let job = 0usize;
        let pid = child.id().unwrap_or(0);
        self.tail.lock().clear();
        let log_path = paths::logs_dir().join("llama-server.log");
        let _ = std::fs::create_dir_all(paths::logs_dir());
        for stream in [child.stdout.take().map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>), child.stderr.take().map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>)].into_iter().flatten() {
            let tail = self.tail.clone();
            let log_path = log_path.clone();
            tokio::spawn(async move {
                let mut r = BufReader::new(stream);
                let mut buf = Vec::new();
                let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&log_path).ok();
                loop {
                    buf.clear();
                    match r.read_until(b'\n', &mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            let line = String::from_utf8_lossy(&buf).trim_end().to_string();
                            if let Some(f) = file.as_mut() {
                                use std::io::Write;
                                let _ = writeln!(f, "{line}");
                            }
                            let mut t = tail.lock();
                            t.push_back(line);
                            while t.len() > 200 {
                                t.pop_front();
                            }
                        }
                    }
                }
            });
        }
        let info = RunningServer { model_path: model.display().to_string(), port, pid, started_at: health_core::util::now_ts(), ready: false };
        *self.server.lock().await = Some(ServerProc { child, job, info: info.clone() });
        self.state.lock().running = Some(info);
        let started = std::time::Instant::now();
        let url = format!("http://127.0.0.1:{port}/health");
        loop {
            {
                let mut g = self.server.lock().await;
                if let Some(p) = g.as_mut() {
                    if let Ok(Some(st)) = p.child.try_wait() {
                        let tail = self.log_tail();
                        *g = None;
                        self.state.lock().running = None;
                        return Err(anyhow!(start_error(st.code(), &tail)));
                    }
                }
            }
            if let Ok(r) = self.http.get(&url).timeout(Duration::from_secs(2)).send().await {
                if r.status().is_success() {
                    break;
                }
            }
            let secs = started.elapsed().as_secs();
            if secs > 300 {
                self.stop().await;
                bail!(t!("Модель загружалась дольше 5 минут — остановлено", "The model took more than 5 minutes to load — stopped"));
            }
            progress(tf!("Загружаю модель в память… {} с", "Loading the model into memory… {}s", secs));
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        if let Some(p) = self.server.lock().await.as_mut() {
            p.info.ready = true;
            self.state.lock().running = Some(p.info.clone());
        }
        hlog!("llama-server ready on port {port}");
        Ok(port)
    }
}

fn start_error(code: Option<i32>, tail: &[String]) -> String {
    let joined = tail.iter().rev().take(6).rev().cloned().collect::<Vec<_>>().join("\n");
    let low = joined.to_lowercase();
    let hint = if low.contains("out of memory") || low.contains("failed to allocate") || low.contains("cudamalloc") {
        t!("Не хватает памяти — уменьшите контекст или число слоёв на GPU, либо выберите модель меньше", "Out of memory — lower the context or GPU layers, or pick a smaller model")
    } else if low.contains("cuda") || low.contains("ggml_cuda") {
        t!("Проблема с CUDA — попробуйте движок Vulkan или CPU в настройках /localmodel", "CUDA problem — try the Vulkan or CPU backend in /localmodel settings")
    } else if low.contains("vulkan") {
        t!("Проблема с Vulkan — обновите драйвер видеокарты или выберите CPU", "Vulkan problem — update the GPU driver or choose CPU")
    } else {
        t!("Подробности в логе llama-server", "See the llama-server log for details")
    };
    format!(
        "{} ({}). {hint}\n{joined}",
        t!("llama-server завершился при запуске", "llama-server exited during startup"),
        code.map(|c| c.to_string()).unwrap_or_else(|| "?".into())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn picks_real_assets() {
        let v: Value = serde_json::from_str(include_str!("../tests/fixtures/gh_releases.json")).unwrap();
        let (tag, a) = pick_assets(&v, LocalBackend::Cuda, true).unwrap();
        assert!(tag.starts_with('b'));
        assert!(a[0].name.contains("cuda-13") && a[0].name.ends_with("-x64.zip"), "{:?}", a);
        assert!(a.len() == 2 && a[1].name.starts_with("cudart-"));
        let (_, a) = pick_assets(&v, LocalBackend::Cuda, false).unwrap();
        assert!(a[0].name.contains("cuda-12"));
        let (_, a) = pick_assets(&v, LocalBackend::Vulkan, true).unwrap();
        assert!(a[0].name.ends_with("-bin-win-vulkan-x64.zip"));
        let (_, a) = pick_assets(&v, LocalBackend::Cpu, true).unwrap();
        assert!(a[0].name.ends_with("-bin-win-cpu-x64.zip"));
    }
}

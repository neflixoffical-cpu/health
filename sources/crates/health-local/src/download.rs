//! Resumable downloads (HTTP Range) with SHA-256 verification, one at a time (FIFO).

use crate::catalog::RemoteFile;
use anyhow::{anyhow, bail, Result};
use futures_util::StreamExt;
use health_core::{hlog, t, tf};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

pub type DownloadId = u64;

#[derive(Clone, Debug, PartialEq)]
pub enum DlState {
    Queued,
    Downloading,
    Verifying,
    Done,
    Failed(String),
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct DownloadState {
    pub id: DownloadId,
    pub label: String,
    pub repo: String,
    pub dest_dir: PathBuf,
    /// Final path of the main (first) model file.
    pub target: PathBuf,
    pub total_bytes: u64,
    pub done_bytes: u64,
    pub speed_bps: f64,
    pub eta_secs: Option<u64>,
    pub state: DlState,
    pub current_file: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DownloadRequest {
    pub repo: String,
    pub files: Vec<RemoteFile>,
    pub mmproj: Option<RemoteFile>,
    pub label: String,
    pub dest_dir: PathBuf,
    /// Override of `https://huggingface.co` (tests).
    pub base_url: Option<String>,
}

/// Local file name for a remote path (flattened).
pub fn local_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// Folder name for a repo: `owner__name`.
pub fn repo_dir(repo: &str) -> String {
    repo.replace('/', "__")
}

#[derive(Clone)]
pub struct Downloads {
    items: Arc<Mutex<Vec<DownloadState>>>,
    cancels: Arc<Mutex<HashMap<DownloadId, CancellationToken>>>,
    queue: Arc<Mutex<VecDeque<(DownloadId, DownloadRequest)>>>,
    next: Arc<AtomicU64>,
    running: Arc<AtomicBool>,
    version: Arc<watch::Sender<u64>>,
    http: reqwest::Client,
}

impl Default for Downloads {
    fn default() -> Self {
        Self::new()
    }
}

impl Downloads {
    pub fn new() -> Self {
        let (tx, _) = watch::channel(0);
        Downloads {
            items: Arc::new(Mutex::new(Vec::new())),
            cancels: Arc::new(Mutex::new(HashMap::new())),
            queue: Arc::new(Mutex::new(VecDeque::new())),
            next: Arc::new(AtomicU64::new(1)),
            running: Arc::new(AtomicBool::new(false)),
            version: Arc::new(tx),
            http: crate::hf::client(),
        }
    }

    fn bump(&self) {
        self.version.send_modify(|v| *v += 1);
    }

    pub fn version(&self) -> watch::Receiver<u64> {
        self.version.subscribe()
    }

    pub fn list(&self) -> Vec<DownloadState> {
        self.items.lock().clone()
    }

    /// Remove finished/failed/cancelled entries from the list.
    pub fn clear_finished(&self) {
        self.items.lock().retain(|d| matches!(d.state, DlState::Queued | DlState::Downloading | DlState::Verifying));
        self.bump();
    }

    pub fn is_active(&self) -> bool {
        self.items.lock().iter().any(|d| matches!(d.state, DlState::Queued | DlState::Downloading | DlState::Verifying))
    }

    pub fn enqueue(&self, req: DownloadRequest) -> DownloadId {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let total: u64 = req.files.iter().map(|f| f.size).sum::<u64>() + req.mmproj.as_ref().map(|m| m.size).unwrap_or(0);
        let target = req.dest_dir.join(local_name(&req.files.first().map(|f| f.path.clone()).unwrap_or_default()));
        self.items.lock().push(DownloadState {
            id,
            label: req.label.clone(),
            repo: req.repo.clone(),
            dest_dir: req.dest_dir.clone(),
            target,
            total_bytes: total,
            done_bytes: 0,
            speed_bps: 0.0,
            eta_secs: None,
            state: DlState::Queued,
            current_file: None,
        });
        self.cancels.lock().insert(id, CancellationToken::new());
        self.queue.lock().push_back((id, req));
        self.bump();
        if !self.running.swap(true, Ordering::SeqCst) {
            let me = self.clone();
            tokio::spawn(async move { me.worker().await });
        }
        id
    }

    pub fn cancel(&self, id: DownloadId) {
        if let Some(c) = self.cancels.lock().get(&id) {
            c.cancel();
        }
        let mut q = self.queue.lock();
        if let Some(pos) = q.iter().position(|(i, _)| *i == id) {
            q.remove(pos);
            drop(q);
            self.set(id, |d| d.state = DlState::Cancelled);
        }
    }

    fn set(&self, id: DownloadId, f: impl FnOnce(&mut DownloadState)) {
        if let Some(d) = self.items.lock().iter_mut().find(|d| d.id == id) {
            f(d);
        }
        self.bump();
    }

    async fn worker(self) {
        loop {
            let job = self.queue.lock().pop_front();
            let Some((id, req)) = job else {
                self.running.store(false, Ordering::SeqCst);
                // re-check to avoid a race with enqueue()
                if !self.queue.lock().is_empty() && !self.running.swap(true, Ordering::SeqCst) {
                    continue;
                }
                return;
            };
            let cancel = self.cancels.lock().get(&id).cloned().unwrap_or_default();
            self.set(id, |d| d.state = DlState::Downloading);
            let res = self.run_job(id, &req, &cancel).await;
            match res {
                Ok(()) => self.set(id, |d| {
                    d.state = DlState::Done;
                    d.done_bytes = d.total_bytes;
                    d.speed_bps = 0.0;
                    d.eta_secs = None;
                    d.current_file = None;
                }),
                Err(e) if cancel.is_cancelled() => {
                    hlog!("download {id} cancelled: {e}");
                    self.set(id, |d| d.state = DlState::Cancelled)
                }
                Err(e) => {
                    hlog!("download {id} failed: {e:#}");
                    self.set(id, |d| d.state = DlState::Failed(format!("{e:#}")))
                }
            }
            self.cancels.lock().remove(&id);
        }
    }

    async fn run_job(&self, id: DownloadId, req: &DownloadRequest, cancel: &CancellationToken) -> Result<()> {
        tokio::fs::create_dir_all(&req.dest_dir).await.map_err(|e| anyhow!("{} {}: {e}", t!("Не удалось создать папку", "Cannot create folder"), req.dest_dir.display()))?;
        let mut all: Vec<RemoteFile> = req.files.clone();
        if let Some(m) = &req.mmproj {
            all.push(m.clone());
        }
        // already present bytes count as done
        let mut have = 0u64;
        for f in &all {
            let dest = req.dest_dir.join(local_name(&f.path));
            if std::fs::metadata(&dest).map(|m| m.len() == f.size).unwrap_or(false) {
                have += f.size;
            } else if let Ok(m) = std::fs::metadata(part_path(&dest)) {
                have += m.len().min(f.size);
            }
        }
        let total: u64 = all.iter().map(|f| f.size).sum();
        if let Some(free) = free_space(&req.dest_dir) {
            let need = total.saturating_sub(have);
            if free < need + 256 * 1024 * 1024 {
                bail!(tf!(
                    "Недостаточно места на диске: нужно {}, свободно {}",
                    "Not enough disk space: need {}, free {}",
                    health_core::util::human_bytes(need),
                    health_core::util::human_bytes(free)
                ));
            }
        }
        let base = req.base_url.clone().unwrap_or_else(|| crate::hf::HF.to_string());
        let done_before = Arc::new(AtomicU64::new(0));
        for f in &all {
            let dest = req.dest_dir.join(local_name(&f.path));
            self.set(id, |d| d.current_file = Some(local_name(&f.path)));
            if std::fs::metadata(&dest).map(|m| m.len() == f.size).unwrap_or(false) {
                done_before.fetch_add(f.size, Ordering::Relaxed);
                continue;
            }
            let url = format!("{base}/{}/resolve/main/{}", req.repo, f.path.split('/').map(urlencode).collect::<Vec<_>>().join("/"));
            let mut attempt = 0;
            loop {
                let started = Instant::now();
                let me = self.clone();
                let base_done = done_before.load(Ordering::Relaxed);
                let mut last_tick = Instant::now();
                let mut last_bytes = 0u64;
                let mut speed = 0.0f64;
                let progress = move |file_done: u64, verifying: bool| {
                    let now = Instant::now();
                    let dt = now.duration_since(last_tick).as_secs_f64();
                    if dt >= 0.4 || verifying {
                        let inst = (file_done.saturating_sub(last_bytes)) as f64 / dt.max(0.001);
                        speed = if speed == 0.0 { inst } else { speed * 0.7 + inst * 0.3 };
                        last_tick = now;
                        last_bytes = file_done;
                        let _ = started;
                        me.set(id, |d| {
                            d.done_bytes = base_done + file_done;
                            d.speed_bps = speed;
                            d.eta_secs = if speed > 1.0 { Some(((d.total_bytes.saturating_sub(d.done_bytes)) as f64 / speed) as u64) } else { None };
                            if verifying {
                                d.state = DlState::Verifying;
                            } else if d.state == DlState::Verifying {
                                d.state = DlState::Downloading;
                            }
                        });
                    }
                };
                match fetch_file(&self.http, &url, &dest, f.size, f.sha256.as_deref(), cancel, progress).await {
                    Ok(()) => break,
                    Err(e) if cancel.is_cancelled() => return Err(e),
                    Err(e) if attempt < 3 && !e.to_string().contains("SHA-256") && !e.to_string().contains("HTTP 4") => {
                        attempt += 1;
                        hlog!("download retry {attempt} for {}: {e:#}", f.path);
                        tokio::select! {
                            _ = cancel.cancelled() => return Err(anyhow!("cancelled")),
                            _ = tokio::time::sleep(Duration::from_secs(2 * attempt)) => {}
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
            done_before.fetch_add(f.size, Ordering::Relaxed);
        }
        Ok(())
    }
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn part_path(dest: &Path) -> PathBuf {
    PathBuf::from(format!("{}.part", dest.display()))
}

/// Free bytes on the volume holding `dir` (nearest existing ancestor).
pub fn free_space(dir: &Path) -> Option<u64> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let mut d = dir.to_path_buf();
        while !d.exists() {
            d = d.parent()?.to_path_buf();
        }
        let wide: Vec<u16> = d.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let mut avail: u64 = 0;
        let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, std::ptr::null_mut(), std::ptr::null_mut()) };
        if ok != 0 {
            Some(avail)
        } else {
            None
        }
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        None
    }
}

/// Download one file with resume and optional SHA-256 check. `progress(bytes_of_this_file, verifying)`.
pub async fn fetch_file(
    http: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected: u64,
    sha256: Option<&str>,
    cancel: &CancellationToken,
    mut progress: impl FnMut(u64, bool),
) -> Result<()> {
    let part = part_path(dest);
    let mut offset = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if expected > 0 && offset > expected {
        let _ = std::fs::remove_file(&part);
        offset = 0;
    }
    let mut hasher = Sha256::new();
    if offset > 0 && sha256.is_some() {
        // hash what we already have (blocking IO off the runtime)
        let p = part.clone();
        let (h, n) = tokio::task::spawn_blocking(move || -> Result<(Sha256, u64)> {
            let mut h = Sha256::new();
            let mut f = std::fs::File::open(&p)?;
            let mut buf = vec![0u8; 4 << 20];
            let mut n = 0u64;
            loop {
                let r = f.read(&mut buf)?;
                if r == 0 {
                    break;
                }
                h.update(&buf[..r]);
                n += r as u64;
            }
            Ok((h, n))
        })
        .await??;
        hasher = h;
        offset = n;
        progress(offset, true);
    }
    let mut rq = http.get(url);
    if let Some(tok) = std::env::var("HF_TOKEN").ok().filter(|t| !t.is_empty()) {
        rq = rq.bearer_auth(tok);
    }
    if offset > 0 {
        rq = rq.header(reqwest::header::RANGE, format!("bytes={offset}-"));
    }
    let resp = tokio::select! {
        _ = cancel.cancelled() => bail!("cancelled"),
        r = rq.send() => r.map_err(|e| anyhow!("{}: {e}", t!("Ошибка сети", "Network error")))?,
    };
    let status = resp.status().as_u16();
    if status == 416 && offset == expected && expected > 0 {
        // already complete
    } else if !(200..300).contains(&status) {
        bail!("HTTP {status} — {url}");
    }
    let append = status == 206;
    if !append && offset > 0 {
        offset = 0;
        hasher = Sha256::new();
    }
    let file = std::fs::OpenOptions::new().create(true).append(append).write(true).truncate(!append).open(&part)?;
    let mut w = std::io::BufWriter::with_capacity(8 << 20, file);
    let mut done = offset;
    if status != 416 {
        let mut stream = resp.bytes_stream();
        loop {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => {
                    let _ = w.flush();
                    bail!("cancelled");
                }
                c = tokio::time::timeout(Duration::from_secs(60), stream.next()) => c,
            };
            match chunk {
                Err(_) => {
                    let _ = w.flush();
                    bail!(t!("Загрузка зависла (нет данных 60 с)", "Download stalled (no data for 60 s)"));
                }
                Ok(None) => break,
                Ok(Some(Err(e))) => {
                    let _ = w.flush();
                    bail!("{}: {e}", t!("Обрыв соединения", "Connection dropped"));
                }
                Ok(Some(Ok(bytes))) => {
                    w.write_all(&bytes)?;
                    if sha256.is_some() {
                        hasher.update(&bytes);
                    }
                    done += bytes.len() as u64;
                    progress(done, false);
                }
            }
        }
    }
    w.flush()?;
    drop(w);
    if expected > 0 && done < expected {
        bail!(tf!("Файл скачан не полностью ({} из {})", "Incomplete download ({} of {})", done, expected));
    }
    if let Some(want) = sha256 {
        progress(done, true);
        let got = hex::encode(hasher.finalize());
        if !got.eq_ignore_ascii_case(want) {
            let _ = std::fs::remove_file(&part);
            bail!("SHA-256 mismatch — {}", t!("файл повреждён, скачайте заново", "the file is corrupted, download it again"));
        }
    }
    if dest.exists() {
        let _ = std::fs::remove_file(dest);
    }
    std::fs::rename(&part, dest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn server(data: Arc<Vec<u8>>) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else { break };
                let data = data.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    let head = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                    let start: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("range: bytes=").and_then(|r| r.trim_end_matches('-').parse().ok()))
                        .unwrap_or(0);
                    let body = &data[start.min(data.len())..];
                    let status = if start > 0 { "206 Partial Content" } else { "200 OK" };
                    let h = format!("HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
                    let _ = s.write_all(h.as_bytes()).await;
                    let _ = s.write_all(body).await;
                });
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn resume_and_verify() {
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let sha = hex::encode(Sha256::digest(&data));
        let base = server(Arc::new(data.clone())).await;
        let dir = std::env::temp_dir().join(format!("health-dl-{}", health_core::util::short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("m.gguf");
        // pretend a previous attempt left 100 KB
        std::fs::write(part_path(&dest), &data[..100_000]).unwrap();
        let http = reqwest::Client::new();
        let cancel = CancellationToken::new();
        fetch_file(&http, &format!("{base}/r/resolve/main/m.gguf"), &dest, data.len() as u64, Some(&sha), &cancel, |_, _| {}).await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        // wrong hash → error and .part removed
        let dest2 = dir.join("x.gguf");
        let err = fetch_file(&http, &format!("{base}/x"), &dest2, data.len() as u64, Some("00"), &cancel, |_, _| {}).await.unwrap_err();
        assert!(err.to_string().contains("SHA-256"));
        assert!(!part_path(&dest2).exists());
        // queue end-to-end
        let dl = Downloads::new();
        let id = dl.enqueue(DownloadRequest {
            repo: "o/r".into(),
            files: vec![RemoteFile { path: "sub/q.gguf".into(), size: data.len() as u64, sha256: Some(sha.clone()) }],
            mmproj: None,
            label: "test".into(),
            dest_dir: dir.join("o__r"),
            base_url: Some(base.clone()),
        });
        for _ in 0..100 {
            if dl.list().iter().any(|d| d.id == id && d.state == DlState::Done) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(dl.list()[0].state, DlState::Done);
        assert!(dir.join("o__r").join("q.gguf").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

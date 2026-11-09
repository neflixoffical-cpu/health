//! Hugging Face API: GGUF model search and file listing grouped into downloadable variants.

use crate::catalog::{estimate_for, RemoteFile};
use crate::gguf::{is_aux, quant_of, split_parts};
use anyhow::{anyhow, bail, Result};
use health_core::t;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

pub const HF: &str = "https://huggingface.co";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HfModel {
    pub id: String,
    pub downloads: u64,
    pub likes: u64,
    pub updated: Option<String>,
    pub gated: bool,
    pub pipeline_tag: Option<String>,
    pub tags: Vec<String>,
}

/// One downloadable model inside a repo (single file or a split set) + optional projector.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GgufVariant {
    pub repo: String,
    /// Display name (file name without the split suffix).
    pub name: String,
    pub quant: String,
    pub files: Vec<RemoteFile>,
    pub size_bytes: u64,
    pub mmproj: Option<RemoteFile>,
}

impl GgufVariant {
    pub fn ram_gb(&self, ctx: u32) -> f64 {
        estimate_for(self.size_bytes + self.mmproj.as_ref().map(|m| m.size).unwrap_or(0), ctx)
    }
    pub fn total_download(&self) -> u64 {
        self.size_bytes + self.mmproj.as_ref().map(|m| m.size).unwrap_or(0)
    }
}

pub(crate) fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(health_core::user_agent())
        .connect_timeout(Duration::from_secs(15))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

fn auth(rq: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    match std::env::var("HF_TOKEN").ok().filter(|t| !t.is_empty()) {
        Some(tok) => rq.bearer_auth(tok),
        None => rq,
    }
}

fn parse_models(v: &Value) -> Vec<HfModel> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    Some(HfModel {
                        id: m.get("id").or_else(|| m.get("modelId"))?.as_str()?.to_string(),
                        downloads: m.get("downloads").and_then(|x| x.as_u64()).unwrap_or(0),
                        likes: m.get("likes").and_then(|x| x.as_u64()).unwrap_or(0),
                        updated: m.get("lastModified").and_then(|x| x.as_str()).map(|s| s.to_string()),
                        gated: m.get("gated").map(|g| !matches!(g, Value::Bool(false) | Value::Null)).unwrap_or(false),
                        pipeline_tag: m.get("pipeline_tag").and_then(|x| x.as_str()).map(|s| s.to_string()),
                        tags: m.get("tags").and_then(|x| x.as_array()).map(|t| t.iter().filter_map(|s| s.as_str().map(|s| s.to_string())).collect()).unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Search GGUF repos (most downloaded first). Empty query = popular.
pub async fn search(query: &str, limit: usize) -> Result<Vec<HfModel>> {
    let http = client();
    let mut rq = http
        .get(format!("{HF}/api/models"))
        .query(&[("filter", "gguf"), ("sort", "downloads"), ("direction", "-1"), ("limit", &limit.clamp(1, 100).to_string())])
        .timeout(Duration::from_secs(20));
    if !query.trim().is_empty() {
        rq = rq.query(&[("search", query.trim())]);
    }
    let resp = auth(rq).send().await.map_err(|e| anyhow!("{}: {e}", t!("Нет связи с Hugging Face", "Can't reach Hugging Face")))?;
    if !resp.status().is_success() {
        bail!("Hugging Face: HTTP {}", resp.status().as_u16());
    }
    let v: Value = resp.json().await?;
    Ok(parse_models(&v))
}

/// Group a repo tree into variants.
pub fn variants_from_tree(repo: &str, tree: &Value) -> Vec<GgufVariant> {
    let files: Vec<RemoteFile> = tree
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|f| f.get("type").and_then(|t| t.as_str()) == Some("file"))
                .filter_map(|f| {
                    let path = f.get("path")?.as_str()?.to_string();
                    if !path.to_lowercase().ends_with(".gguf") {
                        return None;
                    }
                    Some(RemoteFile {
                        size: f.get("size").and_then(|s| s.as_u64()).or_else(|| f.pointer("/lfs/size").and_then(|s| s.as_u64())).unwrap_or(0),
                        sha256: f.pointer("/lfs/oid").and_then(|s| s.as_str()).map(|s| s.to_string()),
                        path,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let mut mmprojs: Vec<RemoteFile> = files.iter().filter(|f| f.path.to_lowercase().contains("mmproj")).cloned().collect();
    mmprojs.sort_by_key(|m| {
        let l = m.path.to_lowercase();
        if l.contains("f16") && !l.contains("bf16") {
            0
        } else if l.contains("bf16") {
            1
        } else if l.contains("q8") {
            2
        } else {
            3
        }
    });
    let mmproj = mmprojs.first().cloned();
    let mut groups: BTreeMap<String, Vec<(u32, RemoteFile)>> = BTreeMap::new();
    for f in files.into_iter().filter(|f| !is_aux(&f.path)) {
        match split_parts(&f.path) {
            Some((prefix, part, _)) => groups.entry(prefix).or_default().push((part, f)),
            None => groups.entry(f.path.trim_end_matches(".gguf").to_string()).or_default().push((1, f)),
        }
    }
    let mut out: Vec<GgufVariant> = groups
        .into_iter()
        .map(|(key, mut parts)| {
            parts.sort_by_key(|p| p.0);
            let files: Vec<RemoteFile> = parts.into_iter().map(|p| p.1).collect();
            let name = key.rsplit('/').next().unwrap_or(&key).to_string();
            GgufVariant {
                repo: repo.to_string(),
                quant: quant_of(&files[0].path),
                size_bytes: files.iter().map(|f| f.size).sum(),
                files,
                name,
                mmproj: mmproj.clone(),
            }
        })
        .collect();
    out.sort_by_key(|v| v.size_bytes);
    out
}

/// List downloadable GGUF variants of a repo.
pub async fn files(repo: &str) -> Result<Vec<GgufVariant>> {
    let http = client();
    let resp = auth(http.get(format!("{HF}/api/models/{repo}/tree/main")).query(&[("recursive", "1")]).timeout(Duration::from_secs(25)))
        .send()
        .await
        .map_err(|e| anyhow!("{}: {e}", t!("Нет связи с Hugging Face", "Can't reach Hugging Face")))?;
    match resp.status().as_u16() {
        200 => {}
        401 | 403 => bail!(t!("Репозиторий закрыт или требует принять лицензию на huggingface.co (можно задать HF_TOKEN)", "The repo is gated or private — accept the license on huggingface.co (or set HF_TOKEN)")),
        404 => bail!(t!("Репозиторий не найден", "Repository not found")),
        s => bail!("Hugging Face: HTTP {s}"),
    }
    let v: Value = resp.json().await?;
    let vars = variants_from_tree(repo, &v);
    if vars.is_empty() {
        bail!(t!("В репозитории нет GGUF-файлов", "No GGUF files in this repository"));
    }
    Ok(vars)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn groups_split_sets() {
        let tree: Value = serde_json::from_str(include_str!("../tests/fixtures/hf_tree_glm.json")).unwrap();
        let v = variants_from_tree("unsloth/GLM-4.5-Air-GGUF", &tree);
        let q4 = v.iter().find(|x| x.quant == "Q4_K_M").expect("Q4_K_M variant");
        assert_eq!(q4.files.len(), 2);
        assert!(q4.size_bytes > 70_000_000_000);
        assert!(q4.files.iter().all(|f| f.sha256.is_some()));
        assert!(v.windows(2).all(|w| w[0].size_bytes <= w[1].size_bytes));
    }
}

//! Minimal GGUF (v2/v3) metadata reader and file-name helpers.

use anyhow::{bail, Result};
use std::io::{BufReader, Read};
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct GgufMeta {
    pub architecture: Option<String>,
    pub name: Option<String>,
    pub block_count: Option<u64>,
    pub context_length: Option<u64>,
    pub embedding_length: Option<u64>,
    pub file_type: Option<u64>,
}

fn rd<const N: usize>(r: &mut impl Read) -> Result<[u8; N]> {
    let mut b = [0u8; N];
    r.read_exact(&mut b)?;
    Ok(b)
}
fn u32_(r: &mut impl Read) -> Result<u32> {
    Ok(u32::from_le_bytes(rd::<4>(r)?))
}
fn u64_(r: &mut impl Read) -> Result<u64> {
    Ok(u64::from_le_bytes(rd::<8>(r)?))
}
fn string(r: &mut impl Read) -> Result<String> {
    let n = u64_(r)? as usize;
    if n > 64 * 1024 * 1024 {
        bail!("string too long");
    }
    let mut b = vec![0u8; n];
    r.read_exact(&mut b)?;
    Ok(String::from_utf8_lossy(&b).into_owned())
}
fn skip(r: &mut impl Read, n: u64) -> Result<()> {
    std::io::copy(&mut r.take(n), &mut std::io::sink())?;
    Ok(())
}

enum Val {
    Int(u64),
    Str(String),
    Other,
}

fn value(r: &mut impl Read, ty: u32) -> Result<Val> {
    Ok(match ty {
        0 | 1 | 7 => Val::Int(rd::<1>(r)?[0] as u64),
        2 | 3 => Val::Int(u16::from_le_bytes(rd::<2>(r)?) as u64),
        4 | 5 => Val::Int(u32_(r)? as u64),
        6 => {
            skip(r, 4)?;
            Val::Other
        }
        8 => Val::Str(string(r)?),
        9 => {
            let ety = u32_(r)?;
            let n = u64_(r)?;
            match ety {
                0 | 1 | 7 => skip(r, n)?,
                2 | 3 => skip(r, n * 2)?,
                4 | 5 | 6 => skip(r, n * 4)?,
                10..=12 => skip(r, n * 8)?,
                8 => {
                    for _ in 0..n {
                        let len = u64_(r)?;
                        skip(r, len)?;
                    }
                }
                _ => bail!("nested arrays are not supported"),
            }
            Val::Other
        }
        10 | 11 => Val::Int(u64_(r)?),
        12 => {
            skip(r, 8)?;
            Val::Other
        }
        t => bail!("unknown GGUF value type {t}"),
    })
}

/// Read the metadata key/values from the start of a GGUF file.
pub fn read_meta(path: &Path) -> Result<GgufMeta> {
    let f = std::fs::File::open(path)?;
    let mut r = BufReader::with_capacity(1 << 20, f);
    if &rd::<4>(&mut r)? != b"GGUF" {
        bail!("not a GGUF file");
    }
    let ver = u32_(&mut r)?;
    if !(2..=3).contains(&ver) {
        bail!("unsupported GGUF version {ver}");
    }
    let _tensors = u64_(&mut r)?;
    let kv = u64_(&mut r)?;
    let mut m = GgufMeta::default();
    let mut ints: Vec<(String, u64)> = Vec::new();
    for _ in 0..kv.min(100_000) {
        let key = string(&mut r)?;
        let ty = u32_(&mut r)?;
        match value(&mut r, ty)? {
            Val::Str(s) => match key.as_str() {
                "general.architecture" => m.architecture = Some(s),
                "general.name" => m.name = Some(s),
                _ => {}
            },
            Val::Int(v) => {
                if key == "general.file_type" {
                    m.file_type = Some(v);
                } else if key.ends_with(".block_count") || key.ends_with(".context_length") || key.ends_with(".embedding_length") {
                    ints.push((key, v));
                }
            }
            Val::Other => {}
        }
    }
    if let Some(a) = &m.architecture {
        for (k, v) in ints {
            if k == format!("{a}.block_count") {
                m.block_count = Some(v);
            } else if k == format!("{a}.context_length") {
                m.context_length = Some(v);
            } else if k == format!("{a}.embedding_length") {
                m.embedding_length = Some(v);
            }
        }
    }
    Ok(m)
}

/// `(prefix, part, total)` for split files like `name-00001-of-00003.gguf`.
pub fn split_parts(file: &str) -> Option<(String, u32, u32)> {
    let base = file.strip_suffix(".gguf").or_else(|| file.strip_suffix(".GGUF"))?;
    let (head, total) = base.rsplit_once("-of-")?;
    let (prefix, part) = head.rsplit_once('-')?;
    if part.len() == 5 && total.len() == 5 {
        Some((prefix.to_string(), part.parse().ok()?, total.parse().ok()?))
    } else {
        None
    }
}

/// Quantization label from a file name (`Q4_K_M`, `UD-Q4_K_XL`, `MXFP4_MOE`, `F16`…).
pub fn quant_of(file: &str) -> String {
    let base = file.rsplit(['/', '\\']).next().unwrap_or(file).trim_end_matches(".gguf");
    let upper = base.to_uppercase();
    let tokens: Vec<&str> = upper.split(['-', '.']).collect();
    for (i, t) in tokens.iter().enumerate().rev() {
        let t = *t;
        let is_q = (t.starts_with("Q") || t.starts_with("IQ") || t.starts_with("TQ")) && t.chars().nth(1).map(|c| c.is_ascii_digit() || c == 'Q').unwrap_or(false)
            || t.starts_with("MXFP4")
            || matches!(t, "F16" | "BF16" | "F32" | "FP16" | "FP8");
        if is_q {
            if i > 0 && tokens[i - 1] == "UD" {
                return format!("UD-{t}");
            }
            return t.to_string();
        }
    }
    "?".to_string()
}

/// Auxiliary files that are not standalone models.
pub fn is_aux(file: &str) -> bool {
    let n = file.rsplit(['/', '\\']).next().unwrap_or(file).to_lowercase();
    n.starts_with("mmproj") || n.contains("-mmproj") || n.starts_with("mtp-") || n.starts_with("dflash-") || n.starts_with("eagle") || n.contains("imatrix")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put_str(v: &mut Vec<u8>, s: &str) {
        v.extend_from_slice(&(s.len() as u64).to_le_bytes());
        v.extend_from_slice(s.as_bytes());
    }

    #[test]
    fn parses_synthetic_header() {
        let mut v = b"GGUF".to_vec();
        v.extend_from_slice(&3u32.to_le_bytes());
        v.extend_from_slice(&0u64.to_le_bytes());
        v.extend_from_slice(&5u64.to_le_bytes());
        put_str(&mut v, "general.architecture");
        v.extend_from_slice(&8u32.to_le_bytes());
        put_str(&mut v, "llama");
        put_str(&mut v, "tokenizer.ggml.tokens");
        v.extend_from_slice(&9u32.to_le_bytes());
        v.extend_from_slice(&8u32.to_le_bytes());
        v.extend_from_slice(&2u64.to_le_bytes());
        put_str(&mut v, "a");
        put_str(&mut v, "b");
        put_str(&mut v, "llama.block_count");
        v.extend_from_slice(&4u32.to_le_bytes());
        v.extend_from_slice(&32u32.to_le_bytes());
        put_str(&mut v, "llama.context_length");
        v.extend_from_slice(&10u32.to_le_bytes());
        v.extend_from_slice(&131072u64.to_le_bytes());
        put_str(&mut v, "general.name");
        v.extend_from_slice(&8u32.to_le_bytes());
        put_str(&mut v, "Test Model");
        let p = std::env::temp_dir().join(format!("t-{}.gguf", health_core::util::short_id()));
        std::fs::write(&p, v).unwrap();
        let m = read_meta(&p).unwrap();
        assert_eq!(m.architecture.as_deref(), Some("llama"));
        assert_eq!(m.block_count, Some(32));
        assert_eq!(m.context_length, Some(131072));
        assert_eq!(m.name.as_deref(), Some("Test Model"));
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn names() {
        assert_eq!(split_parts("Q4_K_M/GLM-4.5-Air-Q4_K_M-00001-of-00002.gguf"), Some(("Q4_K_M/GLM-4.5-Air-Q4_K_M".into(), 1, 2)));
        assert_eq!(split_parts("model.gguf"), None);
        assert_eq!(quant_of("Qwen3-8B-Q4_K_M.gguf"), "Q4_K_M");
        assert_eq!(quant_of("gemma-4-26B-A4B-it-UD-Q4_K_M.gguf"), "UD-Q4_K_M");
        assert_eq!(quant_of("gpt-oss-20b-MXFP4.gguf"), "MXFP4");
        assert_eq!(quant_of("Phi-4-mini-instruct.Q8_0.gguf"), "Q8_0");
        assert_eq!(quant_of("x-IQ4_XS.gguf"), "IQ4_XS");
        assert!(is_aux("mmproj-F16.gguf") && is_aux("MTP/mtp-x.gguf") && !is_aux("qwen.gguf"));
    }
}

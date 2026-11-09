//! The embedded, zlib-compressed `health.exe` (produced by build.rs).

use anyhow::{bail, Result};

include!(concat!(env!("OUT_DIR"), "/payload_meta.rs"));

static PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/payload.bin"));

/// True when this installer was built with a health.exe inside.
pub fn available() -> bool {
    HAS_PAYLOAD && !PAYLOAD.is_empty()
}

/// Uncompressed size of health.exe.
pub fn raw_len() -> usize {
    PAYLOAD_RAW_LEN
}

/// Decompress health.exe and sanity-check it.
pub fn unpack() -> Result<Vec<u8>> {
    if !available() {
        bail!(health_core::t!(
            "Этот установщик собран без health.exe — соберите его через build.ps1",
            "This installer was built without health.exe — build it with build.ps1"
        ));
    }
    let data = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(PAYLOAD, PAYLOAD_RAW_LEN + 1).map_err(|e| anyhow::anyhow!("payload: {e:?}"))?;
    if data.len() != PAYLOAD_RAW_LEN || data.len() < 2 || &data[..2] != b"MZ" {
        bail!(health_core::t!("Повреждённый установщик", "Corrupted installer"));
    }
    Ok(data)
}

//! Embeds health.exe (zlib-compressed) into the installer.
//!
//! Payload source, in order: env `HEALTH_PAYLOAD` (path to health.exe — build.ps1 sets it),
//! else `<target>/<profile>/health.exe` next to this build. Without a payload the installer
//! still builds (for development) and tells the user it is empty.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=HEALTH_PAYLOAD");
    println!("cargo:rerun-if-changed=build.rs");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));

    let candidate = std::env::var_os("HEALTH_PAYLOAD")
        .map(PathBuf::from)
        .or_else(|| out.ancestors().nth(3).map(|p| p.join("health.exe")))
        .filter(|p| p.is_file());

    let (bytes, raw_len) = match &candidate {
        Some(p) => {
            println!("cargo:rerun-if-changed={}", p.display());
            let raw = std::fs::read(p).expect("read payload");
            let packed = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 9);
            (packed, raw.len())
        }
        None => {
            println!("cargo:warning=health-setup: no health.exe payload (set HEALTH_PAYLOAD or build `health` first)");
            (Vec::new(), 0)
        }
    };
    std::fs::write(out.join("payload.bin"), &bytes).expect("write payload.bin");
    std::fs::write(
        out.join("payload_meta.rs"),
        format!("pub const HAS_PAYLOAD: bool = {};\npub const PAYLOAD_RAW_LEN: usize = {};\n", !bytes.is_empty(), raw_len),
    )
    .expect("write payload_meta.rs");

    health_icon::build::embed_windows_resources(&health_icon::build::ResourceOptions {
        product_name: "Health".into(),
        file_description: "Health Setup".into(),
        version: std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "1.0.0".into()),
        original_filename: "HealthSetup.exe".into(),
    });
}

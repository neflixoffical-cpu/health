fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    health_icon::build::embed_windows_resources(&health_icon::build::ResourceOptions {
        product_name: "Health".into(),
        file_description: "Health - AI assistant for your terminal".into(),
        version: std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "1.0.0".into()),
        original_filename: "health.exe".into(),
    });
}

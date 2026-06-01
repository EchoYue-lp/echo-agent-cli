fn main() {
    // Only run tauri_build when the Tauri config exists.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let tauri_config = std::path::Path::new(&manifest_dir).join("tauri.conf.json");
    if tauri_config.exists() {
        tauri_build::build()
    }
}

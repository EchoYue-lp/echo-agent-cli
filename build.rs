fn main() {
    // Only run tauri_build when the tauri binary is being built.
    // We detect this by checking if tauri.conf.json exists in the project root.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let tauri_config = std::path::Path::new(&manifest_dir).join("tauri.conf.json");
    if tauri_config.exists() {
        tauri_build::build()
    }
}

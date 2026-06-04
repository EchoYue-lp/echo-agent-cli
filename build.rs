fn main() {
    // Only run tauri_build when the gui feature is enabled.
    // Cargo sets CARGO_FEATURE_<NAME> for each enabled feature.
    if std::env::var("CARGO_FEATURE_GUI").is_ok() {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
        let tauri_config = std::path::Path::new(&manifest_dir).join("tauri.conf.json");
        if tauri_config.exists() {
            tauri_build::build()
        }
    }
}

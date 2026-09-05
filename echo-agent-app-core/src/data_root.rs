//! EKO's process-wide product data root.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static EKO_DATA_ROOT: OnceLock<PathBuf> = OnceLock::new();

fn default_root() -> PathBuf {
    if let Some(root) = std::env::var_os("EKO_DATA_DIR") {
        let root = PathBuf::from(root);
        if root.is_absolute() {
            return root;
        }
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".eko")
}

pub fn configure(root: impl Into<PathBuf>) -> Result<(), PathBuf> {
    let root = root.into();
    match EKO_DATA_ROOT.set(root.clone()) {
        Ok(()) => Ok(()),
        Err(_) => {
            let current = user_data_dir();
            if current == root {
                Ok(())
            } else {
                Err(current)
            }
        }
    }
}

pub fn user_data_dir() -> PathBuf {
    EKO_DATA_ROOT.get_or_init(default_root).clone()
}

pub fn user_data_path(child: impl AsRef<Path>) -> PathBuf {
    user_data_dir().join(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_root_is_eko_owned() {
        assert!(user_data_dir().ends_with(".eko") || std::env::var_os("EKO_DATA_DIR").is_some());
    }

    #[test]
    fn environment_override_selects_the_default_app_core_root() -> Result<(), String> {
        if let Some(expected) = std::env::var_os("EKO_DATA_DIR") {
            let expected = PathBuf::from(expected);
            let root = default_root();
            if expected.is_absolute() && root != expected {
                return Err(format!(
                    "EKO_DATA_DIR {} did not select app-core root {}",
                    expected.display(),
                    root.display()
                ));
            }
        }
        Ok(())
    }
}

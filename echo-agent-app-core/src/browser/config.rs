use std::path::PathBuf;

const DEFAULT_PACKAGE: &str = "@playwright/mcp@latest";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserConfig {
    pub enabled: bool,
    pub node_command: String,
    pub npm_command: String,
    pub npx_command: String,
    pub package: String,
    pub user_data_dir: PathBuf,
    pub output_dir: PathBuf,
    pub session_dir: PathBuf,
    pub headless: bool,
    pub startup_timeout_secs: u64,
}

impl BrowserConfig {
    pub fn from_env() -> Self {
        let mut config = Self::default();
        config.enabled = env_bool("EKO_BROWSER_ENABLED").unwrap_or(config.enabled);
        config.headless = env_bool("EKO_BROWSER_HEADLESS").unwrap_or(config.headless);
        config.node_command = env_non_empty("EKO_BROWSER_NODE").unwrap_or(config.node_command);
        config.npm_command = env_non_empty("EKO_BROWSER_NPM").unwrap_or(config.npm_command);
        config.npx_command = env_non_empty("EKO_BROWSER_NPX").unwrap_or(config.npx_command);
        config.package = env_non_empty("EKO_BROWSER_MCP_PACKAGE").unwrap_or(config.package);
        if let Some(path) = env_non_empty("EKO_BROWSER_PROFILE_DIR") {
            config.user_data_dir = PathBuf::from(path);
        }
        if let Some(path) = env_non_empty("EKO_BROWSER_OUTPUT_DIR") {
            config.output_dir = PathBuf::from(path);
        }
        if let Some(path) = env_non_empty("EKO_BROWSER_SESSION_DIR") {
            config.session_dir = PathBuf::from(path);
        }
        if let Some(timeout) = env_non_empty("EKO_BROWSER_STARTUP_TIMEOUT_SECS")
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
        {
            config.startup_timeout_secs = timeout;
        }
        config
    }

    pub fn sidecar_args(&self) -> Vec<String> {
        let mut args = vec![
            "-y".to_string(),
            self.package.clone(),
            "--user-data-dir".to_string(),
            self.user_data_dir.to_string_lossy().into_owned(),
            "--caps".to_string(),
            "vision,devtools".to_string(),
        ];
        if self.headless {
            args.push("--headless".to_string());
        }
        args
    }
}

impl Default for BrowserConfig {
    fn default() -> Self {
        let base_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".echo-agent")
            .join("browser");
        Self {
            enabled: true,
            node_command: "node".to_string(),
            npm_command: "npm".to_string(),
            npx_command: "npx".to_string(),
            package: DEFAULT_PACKAGE.to_string(),
            user_data_dir: base_dir.join("profiles").join("managed"),
            output_dir: base_dir.join("output"),
            session_dir: base_dir.join("sessions"),
            headless: false,
            startup_timeout_secs: 60,
        }
    }
}

fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_bool(name: &str) -> Option<bool> {
    match env_non_empty(name)?.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_args_use_managed_profile_and_output() {
        let config = BrowserConfig {
            package: "@playwright/mcp@test".to_string(),
            user_data_dir: PathBuf::from("/tmp/eko-browser-profile"),
            output_dir: PathBuf::from("/tmp/eko-browser-output"),
            headless: true,
            ..BrowserConfig::default()
        };

        assert_eq!(
            config.sidecar_args(),
            vec![
                "-y",
                "@playwright/mcp@test",
                "--user-data-dir",
                "/tmp/eko-browser-profile",
                "--caps",
                "vision,devtools",
                "--headless",
            ]
        );
    }
}

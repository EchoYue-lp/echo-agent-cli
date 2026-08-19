//! EKO-owned managed Python runtime for file-backed analyses.
//!
//! Package policy and provisioning live in the application. Actual scripts
//! continue to execute through the framework `run_code` sandbox.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;
use std::time::Duration;

use echo_core::tools::ScriptExecutionProfile;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const PYPROJECT: &[u8] = include_bytes!("../resources/analytics-runtime/pyproject.toml");
const UV_LOCK: &[u8] = include_bytes!("../resources/analytics-runtime/uv.lock");
const PYTHON_VERSION: &[u8] = include_bytes!("../resources/analytics-runtime/.python-version");
const READY_FILE: &str = "runtime-ready.json";
const LOCK_FILE: &str = ".prepare.lock";
const MAX_COMMAND_OUTPUT_CHARS: usize = 8_000;
const UV_VERSION_TIMEOUT: Duration = Duration::from_secs(15);
const UV_SYNC_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const PYTHON_PROBE_TIMEOUT: Duration = Duration::from_secs(30);
const PACKAGE_NAMES: &[&str] = &[
    "matplotlib",
    "openpyxl",
    "pandas",
    "pyarrow",
    "scipy",
    "seaborn",
    "statsmodels",
];

#[derive(Debug, Error)]
pub enum AnalyticsRuntimeError {
    #[error("analytics runtime I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("analytics runtime metadata is invalid: {0}")]
    InvalidMetadata(#[from] serde_json::Error),
    #[error("uv is unavailable; install uv or configure EKO_UV_PATH: {0}")]
    UvUnavailable(String),
    #[error("uv could not prepare the locked analytics environment: {0}")]
    SyncFailed(String),
    #[error("analytics Python probe failed: {0}")]
    ProbeFailed(String),
    #[error("analytics runtime command timed out: {0}")]
    CommandTimedOut(String),
    #[error("analytics runtime task failed: {0}")]
    TaskFailed(String),
}

pub type AnalyticsRuntimeResult<T> = Result<T, AnalyticsRuntimeError>;

#[derive(Clone)]
pub struct PreparedAnalyticsRuntime {
    pub profile: Arc<ScriptExecutionProfile>,
    pub environment: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct AnalyticsRuntime {
    cache_root: PathBuf,
    uv_program: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReadyMarker {
    contract_version: u32,
    profile_id: String,
    python: String,
    base_prefix: String,
    environment: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct ProbeOutput {
    python: String,
    executable: String,
    base_prefix: String,
    packages: BTreeMap<String, String>,
}

impl Default for AnalyticsRuntime {
    fn default() -> Self {
        let uv_program = std::env::var_os("EKO_UV_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("uv"));
        Self {
            cache_root: echo_agent::paths::user_data_path("runtimes").join("analytics"),
            uv_program,
        }
    }
}

impl AnalyticsRuntime {
    #[cfg(test)]
    fn with_cache_root(cache_root: PathBuf) -> Self {
        Self {
            cache_root,
            uv_program: PathBuf::from("uv"),
        }
    }

    pub async fn prepare_python(&self) -> AnalyticsRuntimeResult<PreparedAnalyticsRuntime> {
        let cache_root = self.cache_root.clone();
        let (runtime_dir, profile_id, lock) = tokio::task::spawn_blocking(move || {
            let profile_hash = runtime_hash();
            let profile_id = format!("eko-analytics:{profile_hash}");
            let runtime_dir = cache_root.join(profile_hash);
            fs::create_dir_all(&runtime_dir)?;
            let lock = open_prepare_lock(&runtime_dir)?;
            lock.lock_exclusive()?;
            Ok::<_, AnalyticsRuntimeError>((runtime_dir, profile_id, lock))
        })
        .await
        .map_err(|error| AnalyticsRuntimeError::TaskFailed(error.to_string()))??;

        let result = self.prepare_locked(&runtime_dir, &profile_id).await;
        if let Err(error) = FileExt::unlock(&lock) {
            tracing::warn!(%error, "failed to unlock analytics runtime preparation file");
        }
        result
    }

    async fn prepare_locked(
        &self,
        runtime_dir: &Path,
        profile_id: &str,
    ) -> AnalyticsRuntimeResult<PreparedAnalyticsRuntime> {
        materialize_runtime_files(runtime_dir)?;
        let marker_path = runtime_dir.join(READY_FILE);
        if let Some(marker) = load_ready_marker(&marker_path, profile_id)? {
            let python = PathBuf::from(&marker.python);
            if python.is_file() {
                return Ok(prepared_runtime(runtime_dir, marker));
            }
        }

        let mut version_command = tokio::process::Command::new(&self.uv_program);
        version_command.arg("--version");
        let uv_version = command_output(
            &mut version_command,
            "uv --version",
            UV_VERSION_TIMEOUT,
            AnalyticsRuntimeError::UvUnavailable,
        )
        .await?;
        let mut sync_command = tokio::process::Command::new(&self.uv_program);
        sync_command
            .arg("sync")
            .arg("--project")
            .arg(runtime_dir)
            .arg("--locked")
            .arg("--no-dev")
            .arg("--python")
            .arg("3.12")
            .env("UV_HTTP_TIMEOUT", "300")
            .env("UV_HTTP_RETRIES", "5");
        let sync_output = command_output(
            &mut sync_command,
            "uv sync",
            UV_SYNC_TIMEOUT,
            AnalyticsRuntimeError::SyncFailed,
        )
        .await?;
        tracing::info!(
            profile_id,
            uv = %bounded_text(&uv_version, 200),
            sync = %bounded_text(&sync_output, 1_000),
            "prepared locked EKO analytics runtime"
        );

        let python = venv_python(runtime_dir);
        if !python.is_file() {
            return Err(AnalyticsRuntimeError::SyncFailed(format!(
                "uv completed without creating '{}'",
                python.display()
            )));
        }
        let probe = probe_python(&python).await?;
        let mut environment = BTreeMap::new();
        environment.insert("analytics.profile".to_string(), profile_id.to_string());
        environment.insert("analytics.uv".to_string(), bounded_text(&uv_version, 200));
        environment.insert("python".to_string(), probe.python);
        environment.insert("python.executable".to_string(), probe.executable);
        for (name, version) in probe.packages {
            environment.insert(format!("python.package.{name}"), version);
        }
        let marker = ReadyMarker {
            contract_version: 1,
            profile_id: profile_id.to_string(),
            python: python.to_string_lossy().to_string(),
            base_prefix: probe.base_prefix,
            environment,
        };
        write_json(&marker_path, &marker)?;
        Ok(prepared_runtime(runtime_dir, marker))
    }
}

impl echo_core::tools::ScriptExecutionProfileResolver for AnalyticsRuntime {
    fn resolve<'a>(
        &'a self,
        language: &'a str,
    ) -> futures::future::BoxFuture<
        'a,
        echo_agent::error::Result<Option<Arc<ScriptExecutionProfile>>>,
    > {
        Box::pin(async move {
            if !matches!(
                language.trim().to_ascii_lowercase().as_str(),
                "python" | "python3"
            ) {
                return Ok(None);
            }
            self.prepare_python()
                .await
                .map(|runtime| Some(runtime.profile))
                .map_err(|error| {
                    echo_agent::error::ReactError::Other(format!(
                        "EKO analytics runtime is unavailable: {error}"
                    ))
                })
        })
    }
}

fn runtime_hash() -> String {
    let mut hasher = Sha256::new();
    hasher.update(PYPROJECT);
    hasher.update(UV_LOCK);
    hasher.update(PYTHON_VERSION);
    hex::encode(hasher.finalize())
}

fn open_prepare_lock(runtime_dir: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(runtime_dir.join(LOCK_FILE))
}

fn materialize_runtime_files(runtime_dir: &Path) -> std::io::Result<()> {
    write_if_changed(&runtime_dir.join("pyproject.toml"), PYPROJECT)?;
    write_if_changed(&runtime_dir.join("uv.lock"), UV_LOCK)?;
    write_if_changed(&runtime_dir.join(".python-version"), PYTHON_VERSION)
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if fs::read(path).ok().as_deref() == Some(bytes) {
        return Ok(());
    }
    echo_core::utils::fs::atomic_write(path, bytes)
}

fn load_ready_marker(
    path: &Path,
    expected_profile_id: &str,
) -> AnalyticsRuntimeResult<Option<ReadyMarker>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let marker: ReadyMarker = serde_json::from_slice(&bytes)?;
    if marker.contract_version != 1 || marker.profile_id != expected_profile_id {
        return Ok(None);
    }
    Ok(Some(marker))
}

fn prepared_runtime(runtime_dir: &Path, marker: ReadyMarker) -> PreparedAnalyticsRuntime {
    let mut profile =
        ScriptExecutionProfile::new(marker.profile_id, "python", PathBuf::from(marker.python))
            .with_env("PYTHONUTF8", "1")
            .with_env("MPLBACKEND", "Agg")
            .with_env("MPLCONFIGDIR", ".matplotlib")
            .with_read_only_path(runtime_dir);
    let base_prefix = PathBuf::from(marker.base_prefix);
    if base_prefix.is_absolute() && base_prefix != runtime_dir {
        profile = profile.with_read_only_path(base_prefix);
    }
    PreparedAnalyticsRuntime {
        profile: Arc::new(profile),
        environment: marker.environment,
    }
}

fn venv_python(runtime_dir: &Path) -> PathBuf {
    if cfg!(target_os = "windows") {
        runtime_dir.join(".venv/Scripts/python.exe")
    } else {
        runtime_dir.join(".venv/bin/python")
    }
}

async fn probe_python(python: &Path) -> AnalyticsRuntimeResult<ProbeOutput> {
    let packages = serde_json::to_string(PACKAGE_NAMES)?;
    let script = format!(
        "import importlib.metadata as m, json, sys; names={packages}; print(json.dumps({{'python': sys.version.split()[0], 'executable': sys.executable, 'base_prefix': sys.base_prefix, 'packages': {{name: m.version(name) for name in names}}}}))"
    );
    let mut command = tokio::process::Command::new(python);
    command.arg("-c").arg(script);
    let output = command_raw_output(
        &mut command,
        "analytics Python probe",
        PYTHON_PROBE_TIMEOUT,
        AnalyticsRuntimeError::ProbeFailed,
    )
    .await?;
    serde_json::from_slice(&output.stdout).map_err(AnalyticsRuntimeError::InvalidMetadata)
}

async fn command_output<F>(
    command: &mut tokio::process::Command,
    label: &str,
    timeout: Duration,
    error: F,
) -> AnalyticsRuntimeResult<String>
where
    F: Fn(String) -> AnalyticsRuntimeError,
{
    let output = command_raw_output(command, label, timeout, error).await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(bounded_text(
        &format!("{stdout}\n{stderr}"),
        MAX_COMMAND_OUTPUT_CHARS,
    ))
}

async fn command_raw_output<F>(
    command: &mut tokio::process::Command,
    label: &str,
    timeout: Duration,
    error: F,
) -> AnalyticsRuntimeResult<Output>
where
    F: Fn(String) -> AnalyticsRuntimeError,
{
    command.kill_on_drop(true);
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| {
            AnalyticsRuntimeError::CommandTimedOut(format!(
                "{label} exceeded {} seconds",
                timeout.as_secs()
            ))
        })?
        .map_err(|source| error(source.to_string()))?;
    if !output.status.success() {
        return Err(error(output_message(&output)));
    }
    Ok(output)
}

fn output_message(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    bounded_text(
        &format!(
            "status={} stdout={} stderr={}",
            output.status, stdout, stderr
        ),
        MAX_COMMAND_OUTPUT_CHARS,
    )
}

fn bounded_text(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> AnalyticsRuntimeResult<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    echo_core::utils::fs::atomic_write(path, &bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_runtime_has_a_stable_content_identity() {
        let first = runtime_hash();
        let second = runtime_hash();
        assert_eq!(first, second);
        assert_eq!(first.chars().count(), 64);
        assert!(String::from_utf8_lossy(UV_LOCK).contains("name = \"pandas\""));
    }

    #[test]
    fn materialized_runtime_is_content_addressed_and_idempotent() -> AnalyticsRuntimeResult<()> {
        let root = tempfile::tempdir()?;
        let runtime = AnalyticsRuntime::with_cache_root(root.path().to_path_buf());
        let runtime_dir = runtime.cache_root.join(runtime_hash());
        fs::create_dir_all(&runtime_dir)?;
        materialize_runtime_files(&runtime_dir)?;
        materialize_runtime_files(&runtime_dir)?;

        assert_eq!(fs::read(runtime_dir.join("pyproject.toml"))?, PYPROJECT);
        assert_eq!(fs::read(runtime_dir.join("uv.lock"))?, UV_LOCK);
        assert_eq!(
            fs::read(runtime_dir.join(".python-version"))?,
            PYTHON_VERSION
        );
        Ok(())
    }

    #[test]
    fn ready_marker_builds_a_redacted_sandbox_profile() {
        let runtime_dir = PathBuf::from("/tmp/eko-analytics/hash");
        let marker = ReadyMarker {
            contract_version: 1,
            profile_id: "eko-analytics:hash".to_string(),
            python: "/tmp/eko-analytics/hash/.venv/bin/python".to_string(),
            base_prefix: "/opt/python".to_string(),
            environment: BTreeMap::from([("python".to_string(), "3.12.4".to_string())]),
        };
        let prepared = prepared_runtime(&runtime_dir, marker);
        assert_eq!(prepared.profile.language, "python");
        assert_eq!(
            prepared.profile.env.get("MPLBACKEND").map(String::as_str),
            Some("Agg")
        );
        assert!(prepared.profile.read_only_paths.contains(&runtime_dir));
        assert!(
            prepared
                .profile
                .read_only_paths
                .contains(&PathBuf::from("/opt/python"))
        );
    }

    #[tokio::test]
    async fn unavailable_uv_returns_an_actionable_error() -> AnalyticsRuntimeResult<()> {
        let root = tempfile::tempdir()?;
        let runtime = AnalyticsRuntime {
            cache_root: root.path().to_path_buf(),
            uv_program: root.path().join("missing-uv"),
        };
        let result = runtime.prepare_python().await;
        assert!(matches!(
            result,
            Err(AnalyticsRuntimeError::UvUnavailable(_))
        ));
        Ok(())
    }

    #[tokio::test]
    #[ignore = "provisions the locked EKO analytics environment"]
    async fn live_locked_runtime_provisions_and_probes() -> AnalyticsRuntimeResult<()> {
        let prepared = AnalyticsRuntime::default().prepare_python().await?;
        assert!(prepared.profile.program.is_file());
        assert_eq!(prepared.profile.language, "python");
        assert!(prepared.environment.contains_key("python.package.pandas"));
        assert!(prepared.environment.contains_key("python.package.pyarrow"));
        Ok(())
    }

    #[tokio::test]
    #[ignore = "executes the locked EKO analytics environment through the OS sandbox"]
    async fn live_locked_runtime_executes_through_run_code() -> AnalyticsRuntimeResult<()> {
        let prepared = AnalyticsRuntime::default().prepare_python().await?;
        let workspace = tempfile::tempdir()?;
        fs::write(
            workspace.path().join("分析.py"),
            "import pandas, pyarrow; print(f'pandas={pandas.__version__},pyarrow={pyarrow.__version__}')\n",
        )?;

        let mut manager = echo_agent::tools::ToolManager::new();
        echo_agent::tools::register_all_tools(&mut manager);
        manager.apply_sandbox(Arc::new(
            echo_agent::sandbox::SandboxManager::local_sandbox(),
        ));
        let context = echo_agent::tools::ToolContext {
            working_dir: Some(workspace.path().to_path_buf()),
            script_execution_profile: Some(prepared.profile),
            ..echo_agent::tools::ToolContext::default()
        };
        let parameters = echo_agent::tools::ToolParameters::from([
            ("language".to_string(), serde_json::json!("python")),
            ("script_path".to_string(), serde_json::json!("分析.py")),
        ]);

        let result = manager
            .execute_tool_with_context("run_code", parameters, &context)
            .await
            .map_err(|error| AnalyticsRuntimeError::TaskFailed(error.to_string()))?;
        if !result.success {
            return Err(AnalyticsRuntimeError::TaskFailed(
                result.error.unwrap_or(result.output),
            ));
        }
        assert!(result.output.contains("pandas="));
        assert!(result.output.contains("pyarrow="));
        Ok(())
    }
}

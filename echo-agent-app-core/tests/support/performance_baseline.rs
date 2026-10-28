// Shared only by test harnesses. Product state remains with its existing owners.
use serde_json::{Value, json};
use std::error::Error;
use std::io::Write;
use std::path::{Path, PathBuf};

pub(super) type HarnessResult<T = ()> = Result<T, Box<dyn Error>>;

pub(super) fn required(name: &str) -> HarnessResult<String> {
    let value = std::env::var(name)?;
    if value.trim().is_empty() {
        return Err(format!("{name} is required").into());
    }
    Ok(value)
}

pub(super) fn root() -> HarnessResult<PathBuf> {
    let root = PathBuf::from(required("EKO_PERF_ROOT")?);
    let fixed = PathBuf::from(required("EKO_PERF_FIXED_ROOT")?);
    if !root.is_absolute() || root != fixed {
        return Err("fixture must run at its original absolute isolated root".into());
    }
    Ok(root)
}

pub(super) fn usize_arg(name: &str) -> HarnessResult<usize> {
    Ok(required(name)?.parse()?)
}

pub(super) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> HarnessResult<T> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

pub(super) fn write_json(path: &Path, value: &impl serde::Serialize) -> HarnessResult {
    std::fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

pub(super) fn phase_index() -> HarnessResult<(String, usize)> {
    let phase = required("EKO_PERF_PHASE")?;
    if phase != "warmup" && phase != "measurement" {
        return Err("invalid measurement phase".into());
    }
    Ok((phase, usize_arg("EKO_PERF_SAMPLE_INDEX")?))
}

pub(super) fn emit(
    scenario: &str,
    mode: &str,
    phase: &str,
    index: usize,
    elapsed_ns: u128,
    outcome: &str,
    correctness: Value,
) -> HarnessResult {
    let row = json!({
        "scenario":scenario,"mode":mode,"phase":phase,"sample_index":index,
        "elapsed_ns":elapsed_ns.to_string(),"outcome":outcome,
        "dataset_sha256":required("EKO_PERF_DATASET_SHA256")?,
        "source_sha256":required("EKO_PERF_SOURCE_SHA256")?,
        "protocol_sha256":required("EKO_PERF_PROTOCOL_SHA256")?,
        "environment_id":required("EKO_PERF_ENVIRONMENT_ID")?,
        "process_observation_id":required("EKO_PERF_PROCESS_ID")?,
        "correctness":correctness,
    });
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(required("EKO_PERF_SAMPLES")?)?;
    writeln!(file, "{row}")?;
    Ok(())
}

pub(super) fn empty_seed_root(root: &Path) -> HarnessResult {
    std::fs::create_dir_all(root)?;
    if std::fs::read_dir(root)?.next().is_some() {
        return Err("seed root must be empty".into());
    }
    Ok(())
}

pub(super) fn check_release() -> HarnessResult {
    if cfg!(debug_assertions) {
        return Err("retained release harness required".into());
    }
    Ok(())
}

fn inbox_dir(root: &Path, target: &AgentAddress) -> PathBuf {
    root.join("inboxes")
        .join(stable_segment(target.workspace_id.as_str()))
        .join(stable_segment(&target.conversation_id))
}

fn stable_segment(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn corrupt_event(path: &Path, message: impl Into<String>) -> AgentRouterError {
    AgentRouterError::Corrupt {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

fn journal_error(path: &Path, error: impl std::fmt::Display) -> AgentRouterError {
    corrupt_event(path, error.to_string())
}

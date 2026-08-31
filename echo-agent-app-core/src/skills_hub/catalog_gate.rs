//! Builtin skill catalog gate — the `skills-ref validate` equivalent for the
//! skills shipped under `echo-agent-cli/skills/`.
//!
//! Every bundled skill must pass official agentskills.io validation (standard
//! frontmatter fields only, space-separated `allowed-tools`, string metadata,
//! and the catalog must stay in sync with
//! `BUILTIN_SKILL_NAMES`.

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use echo_agent::skills::external::validate_skill_dir;

    use super::super::enabled_skills::{BUILTIN_SKILL_NAMES, builtin_skills_root};

    fn discover_skill_dirs() -> Vec<PathBuf> {
        let root = builtin_skills_root();
        let mut dirs = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                if path.join("SKILL.md").is_file() {
                    dirs.push(path);
                } else {
                    stack.push(path);
                }
            }
        }
        dirs.sort();
        dirs
    }

    #[test]
    fn builtin_skill_catalog_passes_official_validation() {
        let dirs = discover_skill_dirs();
        assert!(
            !dirs.is_empty(),
            "builtin skills root must contain skills: {}",
            builtin_skills_root().display()
        );

        let mut violations = Vec::new();
        for dir in &dirs {
            let report = validate_skill_dir(dir);
            if !report.is_valid() {
                violations.push(format!("{}: {:#?}", dir.display(), report.violations));
            }
        }
        assert!(
            violations.is_empty(),
            "builtin skill catalog has spec violations:\n{}",
            violations.join("\n")
        );
    }

    #[test]
    fn builtin_skill_catalog_matches_builtin_skill_names() {
        let dirs = discover_skill_dirs();
        let catalog_names: BTreeSet<String> = dirs
            .iter()
            .filter_map(|dir| dir.file_name().and_then(|n| n.to_str()))
            .map(String::from)
            .collect();
        let registered: BTreeSet<String> = BUILTIN_SKILL_NAMES
            .iter()
            .map(|name| name.to_string())
            .collect();

        let missing: Vec<_> = registered.difference(&catalog_names).collect();
        let unregistered: Vec<_> = catalog_names.difference(&registered).collect();
        assert!(
            missing.is_empty() && unregistered.is_empty(),
            "BUILTIN_SKILL_NAMES out of sync with skills/ — missing from disk: {missing:?}; \
             not registered: {unregistered:?}"
        );
    }
}

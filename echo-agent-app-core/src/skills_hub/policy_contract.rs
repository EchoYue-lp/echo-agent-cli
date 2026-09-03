//! Behavior-level contract tests for builtin Skill activation authority.
//!
//! These pin the contract from ADR 0032: `enabled-skills.json` decides which
//! bundled Skills enter an Agent runtime. Disabled builtins must be absent
//! from every runtime projection — descriptors, the progressive activation
//! registry, and private per-skill hook extensions (which must never be
//! present), and (because intent routing is a projection of
//! the committed descriptor catalog) IntentRouter candidates. Standard-format
//! `SKILL.md` files carry no trigger field, so `skill_descriptors()` is the
//! single source the router is rebuilt from.

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use echo_agent::agent::{AgentConfig, ReactAgent};
    use echo_agent::skills::external::SkillLoadPolicy;

    use super::super::enabled_skills::{
        ActiveSkillLoadPolicy, EnabledSkillsConfig, SkillEnableEntry,
    };

    struct TempRoot(PathBuf);
    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_root(label: &str) -> TempRoot {
        let raw = std::env::temp_dir().join(format!(
            "eko_skill_contract_{}_{}_{}",
            label,
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&raw).expect("create temp root");
        // Canonicalized so assertions compare against the loader's
        // canonicalized descriptor locations (symlinks such as /tmp resolved).
        let canonical = raw.canonicalize().unwrap_or(raw);
        TempRoot(canonical)
    }

    fn write_skill(dir: &std::path::Path, name: &str, description: &str) {
        std::fs::create_dir_all(dir).expect("create skill dir");
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\nBody of {name}.\n"),
        )
        .expect("write SKILL.md");
    }

    fn config_with(skills: &[(&str, bool)]) -> EnabledSkillsConfig {
        let mut config = EnabledSkillsConfig {
            skills: HashMap::new(),
            ..EnabledSkillsConfig::default()
        };
        for (name, enabled) in skills {
            config.skills.insert(
                name.to_string(),
                SkillEnableEntry {
                    category: "builtin".into(),
                    enabled: *enabled,
                    baseline: false,
                },
            );
        }
        config
    }

    fn agent_with_policy(
        config_path: &std::path::Path,
        builtin_root: &std::path::Path,
    ) -> ReactAgent {
        let mut agent = ReactAgent::new(AgentConfig::minimal("model", "contract"));
        agent.set_skill_load_policy(Some(Arc::new(ActiveSkillLoadPolicy::new(
            config_path.to_path_buf(),
            builtin_root.to_path_buf(),
            None,
        ))));
        agent
    }

    #[tokio::test]
    async fn disabled_builtin_is_absent_from_every_runtime_projection() {
        let root = temp_root("disabled");
        let builtin = root.0.join("builtin");
        write_skill(&builtin.join("on-skill"), "on-skill", "enabled skill");
        write_skill(&builtin.join("off-skill"), "off-skill", "disabled skill");

        let config_path = root.0.join("enabled-skills.json");
        config_with(&[("on-skill", true), ("off-skill", false)])
            .save(&config_path)
            .expect("save config");

        let mut agent = agent_with_policy(&config_path, &builtin);
        agent
            .load_skills_from_dir(&builtin)
            .await
            .expect("load builtin dir");

        // Descriptor catalog (also the source IntentRouter is rebuilt from).
        let names: Vec<String> = agent
            .skill_descriptors()
            .into_iter()
            .map(|descriptor| descriptor.name)
            .collect();
        assert!(names.contains(&"on-skill".to_string()));
        assert!(!names.contains(&"off-skill".to_string()));

        // Progressive activation registry: the disabled skill cannot activate.
        assert!(!agent.skill_registry_mut().is_installed("off-skill"));
        assert!(agent.skill_registry_mut().is_installed("on-skill"));
        // `activate_skill` surfaces an explicit error for uninstalled skills
        // (Phase 0 contract change); the disabled skill must stay unactivated.
        let rejected = agent
            .activate_skill("off-skill")
            .await
            .expect_err("disabled skill activation must error");
        assert!(rejected.to_string().contains("not installed"));
        assert!(!agent.skill_registry_mut().is_activated("off-skill"));
        agent
            .activate_skill("on-skill")
            .await
            .expect("activate enabled");
        assert!(agent.skill_registry_mut().is_activated("on-skill"));

        // Standard Agent Skills files do not carry private per-skill hooks.
        let hooks = agent.hook_registry().read().await;
        let sources = hooks.list_sources();
        assert!(
            sources
                .iter()
                .all(|(source, _)| source != "skill:off-skill"),
            "disabled builtin hooks must not be registered: {sources:?}"
        );
    }

    #[tokio::test]
    async fn enable_after_disable_registers_full_runtime_on_reload() {
        let root = temp_root("enable");
        let builtin = root.0.join("builtin");
        write_skill(&builtin.join("flippy"), "flippy", "flipping skill");

        let config_path = root.0.join("enabled-skills.json");
        let mut config = config_with(&[("flippy", false)]);
        config.save(&config_path).expect("save config");

        let mut agent = agent_with_policy(&config_path, &builtin);
        agent.load_skills_from_dir(&builtin).await.expect("load");
        assert!(agent.skill_descriptors().is_empty());

        // Enable and reconcile on the same agent instance.
        config.set_enabled("flippy", true);
        config.save(&config_path).expect("resave config");
        agent
            .reload_skills_from_dir(&builtin)
            .await
            .expect("reload builtin dir");
        agent.reconcile_skill_load_policy().await;

        assert!(agent.skill_registry_mut().is_installed("flippy"));
        let hooks = agent.hook_registry().read().await;
        assert!(
            hooks
                .list_sources()
                .iter()
                .all(|(source, _)| !source.starts_with("skill:"))
        );
    }

    #[tokio::test]
    async fn reload_reflects_description_and_hook_changes() {
        let root = temp_root("reload");
        let builtin = root.0.join("builtin");
        let skill_dir = builtin.join("mutable");
        write_skill(&skill_dir, "mutable", "original description");

        let config_path = root.0.join("enabled-skills.json");
        config_with(&[("mutable", true)])
            .save(&config_path)
            .expect("save config");

        let mut agent = agent_with_policy(&config_path, &builtin);
        agent.load_skills_from_dir(&builtin).await.expect("load");
        let original = agent
            .skill_descriptors()
            .into_iter()
            .find(|descriptor| descriptor.name == "mutable")
            .expect("mutable installed");
        assert_eq!(original.description, "original description");
        assert!(original.hooks.is_none());

        // Change description, then reload the directory.
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: mutable\ndescription: updated description\n---\nBody of mutable.\n",
        )
        .expect("rewrite SKILL.md");
        agent
            .reload_skills_from_dir(&builtin)
            .await
            .expect("reload");
        agent.reconcile_skill_load_policy().await;

        let updated = agent
            .skill_descriptors()
            .into_iter()
            .find(|descriptor| descriptor.name == "mutable")
            .expect("mutable reinstalled");
        assert_eq!(updated.description, "updated description");
        assert!(updated.hooks.is_none());
        assert_eq!(agent.skill_descriptors().len(), 1);
    }

    #[test]
    fn malformed_enabled_config_falls_back_to_default_active_set() {
        // 2026-09 语义变更(fail-closed → fail-open):本地个人助理不应因
        // 一个写坏的 JSON 而静默丢失全部内置 skill;坏文件回退默认启用集。
        let root = temp_root("failopen");
        let config_path = root.0.join("enabled-skills.json");
        std::fs::write(&config_path, "{ not valid json").expect("corrupt config");

        let policy = ActiveSkillLoadPolicy::new(config_path, root.0.join("builtin"), None);
        let mut descriptor = echo_agent::skills::external::SkillDocument::parse(
            "---\nname: git-workflow\ndescription: core\n---\nbody",
        )
        .expect("parse")
        .into_descriptor();
        descriptor.location = root.0.join("builtin/git-workflow/SKILL.md");
        assert!(
            policy.allows(&descriptor),
            "corrupted enabled-skills.json must fall back to the default active set"
        );
        let mut optional = echo_agent::skills::external::SkillDocument::parse(
            "---\nname: docx\ndescription: optional\n---\nbody",
        )
        .expect("parse")
        .into_descriptor();
        optional.location = root.0.join("builtin/docx/SKILL.md");
        assert!(
            !policy.allows(&optional),
            "default fallback must still keep opt-in skills disabled"
        );
    }

    #[tokio::test]
    async fn user_skills_bypass_builtin_policy_and_same_name_keeps_first_source() {
        let root = temp_root("user");
        let builtin = root.0.join("builtin");
        let user = root.0.join("user-skills");
        write_skill(
            &builtin.join("shared-name"),
            "shared-name",
            "builtin variant",
        );
        write_skill(&user.join("user-only"), "user-only", "user skill");
        write_skill(&user.join("shared-name"), "shared-name", "user variant");

        // Everything disabled in the config.
        let config_path = root.0.join("enabled-skills.json");
        EnabledSkillsConfig {
            skills: HashMap::new(),
            ..EnabledSkillsConfig::default()
        }
        .save(&config_path)
        .expect("save config");

        let mut agent = agent_with_policy(&config_path, &builtin);
        // Builtin first (mirrors runtime startup order)…
        agent.load_skills_from_dir(&builtin).await.expect("builtin");
        // …then the user tree, which is not governed by the builtin policy.
        agent.load_skills_from_dir(&user).await.expect("user");

        let descriptors = agent.skill_descriptors();
        let shared = descriptors
            .iter()
            .find(|descriptor| descriptor.name == "shared-name")
            .expect("user same-name skill registers when the builtin is disabled");
        assert!(
            shared.location.starts_with(&user),
            "the only live registration must be the user variant"
        );
        assert!(
            descriptors
                .iter()
                .any(|descriptor| descriptor.name == "user-only"),
            "user skills must not be disabled by the builtin activation policy"
        );
        assert_eq!(
            descriptors
                .iter()
                .filter(|descriptor| descriptor.name == "shared-name")
                .count(),
            1
        );

        // With the builtin enabled, the first (builtin) registration wins and
        // a later same-name user skill does not replace it.
        let mut config = EnabledSkillsConfig {
            skills: HashMap::new(),
            ..EnabledSkillsConfig::default()
        };
        config.skills.insert(
            "shared-name".to_string(),
            SkillEnableEntry {
                category: "builtin".into(),
                enabled: true,
                baseline: false,
            },
        );
        config.save(&config_path).expect("resave config");

        let mut agent = agent_with_policy(&config_path, &builtin);
        agent.load_skills_from_dir(&builtin).await.expect("builtin");
        agent.load_skills_from_dir(&user).await.expect("user");
        let shared = agent
            .skill_descriptors()
            .into_iter()
            .find(|descriptor| descriptor.name == "shared-name")
            .expect("shared-name present");
        assert!(
            shared.location.starts_with(&builtin),
            "enabled builtin keeps priority over a later same-name user skill"
        );
    }
}

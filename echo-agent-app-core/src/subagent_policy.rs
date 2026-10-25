//! EKO's product limit. Framework delegation remains independently configurable.

use echo_agent::tasks::NestedDelegationPolicy;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::atomic::{AtomicU8, Ordering};

/// Validated maximum number of Subagent layers below the primary (layer zero).
/// The private value prevents owned config mutations from bypassing validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubagentDepthLimit(u8);

impl SubagentDepthLimit {
    pub const MAX: u8 = 3;

    pub fn new(value: u8) -> Result<Self, String> {
        if (1..=Self::MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err("max_subagent_depth must be an integer from 1 to 3".to_string())
        }
    }

    pub const fn get(self) -> u8 {
        self.0
    }

    pub const fn primary_policy(self) -> NestedDelegationPolicy {
        NestedDelegationPolicy {
            can_spawn_subagents: true,
            delegate_depth: 0,
            max_delegate_depth: self.0,
        }
    }

    /// TaskRuntime's exact context describes its target child, not its primary.
    pub fn first_child_policy(self) -> Result<NestedDelegationPolicy, String> {
        self.primary_policy()
            .child_policy()
            .ok_or_else(|| "validated EKO limit did not permit its first child".to_string())
    }
}

impl Default for SubagentDepthLimit {
    fn default() -> Self {
        Self(1)
    }
}

/// Legacy work may still be observed/cancelled, but a missing binding is not
/// permission to create descendants using today's broader configuration.
pub(crate) fn primary_policy_for_binding(
    binding: Option<SubagentDepthLimit>,
) -> NestedDelegationPolicy {
    binding
        .map(SubagentDepthLimit::primary_policy)
        .unwrap_or(NestedDelegationPolicy {
            can_spawn_subagents: false,
            delegate_depth: 0,
            max_delegate_depth: 0,
        })
}

impl Serialize for SubagentDepthLimit {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.0)
    }
}

impl<'de> Deserialize<'de> for SubagentDepthLimit {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = u8::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Omitted patches mean no edit; explicit null is not a valid integer.
pub(crate) fn deserialize_optional_depth<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<SubagentDepthLimit>, D::Error> {
    SubagentDepthLimit::deserialize(deserializer).map(Some)
}

/// In-memory projection of the last committed EkoConfig value. Stores and
/// workspace hosts share this projection only to capture *new* runs/turns;
/// active execution always uses its immutable journal/context binding.
#[derive(Debug)]
pub(crate) struct SubagentDepthDefaults(AtomicU8);

impl SubagentDepthDefaults {
    pub(crate) fn new(limit: SubagentDepthLimit) -> Self {
        Self(AtomicU8::new(limit.get()))
    }

    pub(crate) fn snapshot(&self) -> SubagentDepthLimit {
        // All writes originate from the validated, privately constructed type.
        SubagentDepthLimit(self.0.load(Ordering::Acquire))
    }

    pub(crate) fn publish(&self, limit: SubagentDepthLimit) {
        self.0.store(limit.get(), Ordering::Release);
    }
}

impl Default for SubagentDepthDefaults {
    fn default() -> Self {
        Self::new(SubagentDepthLimit::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_patch_distinguishes_omission_from_invalid_null()
    -> Result<(), Box<dyn std::error::Error>> {
        let omitted: crate::types::UpdateFullConfigRequest = serde_json::from_str("{}")?;
        assert!(omitted.max_subagent_depth.is_none());
        for invalid in [
            serde_json::Value::Null,
            serde_json::json!(0),
            serde_json::json!(4),
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!("2"),
        ] {
            assert!(
                serde_json::from_value::<crate::types::UpdateFullConfigRequest>(
                    serde_json::json!({"max_subagent_depth": invalid})
                )
                .is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn accepts_only_integer_product_limits() -> Result<(), Box<dyn std::error::Error>> {
        for value in 1..=3 {
            let limit: SubagentDepthLimit = serde_json::from_value(value.into())?;
            assert_eq!(limit.get(), value);
        }
        for value in [
            serde_json::json!(0),
            serde_json::json!(4),
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!("2"),
            serde_json::Value::Null,
        ] {
            assert!(serde_json::from_value::<SubagentDepthLimit>(value).is_err());
        }
        assert_eq!(SubagentDepthLimit::default().get(), 1);
        Ok(())
    }

    #[test]
    fn each_real_spawn_advances_once_and_never_creates_layer_four() -> Result<(), String> {
        for maximum in 1..=3 {
            let limit = SubagentDepthLimit::new(maximum)?;
            let mut actor = limit.primary_policy();
            for expected in 1..=maximum {
                actor = actor
                    .child_policy()
                    .ok_or_else(|| "child rejected early".to_string())?;
                assert_eq!(actor.delegate_depth, expected);
                assert_eq!(actor.max_delegate_depth, maximum);
            }
            assert!(actor.child_policy().is_none());
        }
        Ok(())
    }

    #[test]
    fn config_yaml_round_trip_and_invalid_values_share_the_product_type()
    -> Result<(), Box<dyn std::error::Error>> {
        let missing: crate::config::EkoConfig = serde_yaml::from_str("{}")?;
        assert_eq!(missing.max_subagent_depth.get(), 1);
        for limit in 1..=3 {
            let config: crate::config::EkoConfig =
                serde_yaml::from_str(&format!("max_subagent_depth: {limit}\n"))?;
            let saved = serde_yaml::to_string(&config)?;
            let loaded: crate::config::EkoConfig = serde_yaml::from_str(&saved)?;
            assert_eq!(loaded.max_subagent_depth.get(), limit);
        }
        for invalid in ["0", "4", "-1", "1.5", "\"2\"", "null"] {
            assert!(
                serde_yaml::from_str::<crate::config::EkoConfig>(&format!(
                    "max_subagent_depth: {invalid}\n"
                ))
                .is_err()
            );
        }
        Ok(())
    }
}

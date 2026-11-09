//! EKO-only composition adapter from the original scheduler to ChannelRuntime.
//! It owns neither occurrences nor a second delivery queue/journal.

use std::sync::{Arc, OnceLock, Weak};

use echo_agent::agent::CancellationToken;
use echo_agent::channels::weixin::{WeixinContextRef, WeixinSendOutcome, encode_target};
use echo_agent::channels::{ChatType, OutboundMessage};
use echo_agent::scheduler::SchedulerInvocation;
use futures::future::BoxFuture;

use crate::channel_runtime::ChannelRuntime;
use crate::config::WeixinChannelConfig;
use crate::state::AppState;
use crate::tasks::task_runtime::weixin_delivery::{WeixinCronDelivery, WeixinDeliveryTarget};

struct OriginalOwners {
    state: Weak<AppState>,
    channels: Weak<ChannelRuntime>,
}

/// Scheduler construction precedes Arc<AppState>/ChannelRuntime publication.
/// This one binding closes that composition gap without retaining either root.
/// An early callback waits inside its original scheduler-owned future; it
/// cannot freeze None or fail an ordinary occurrence merely because bootstrap
/// has not published its owners yet. Original scheduler cancellation also ends
/// this wait, including rollback before a ChannelRuntime exists.
#[derive(Default)]
pub(crate) struct WeixinCronBinding {
    owners: OnceLock<OriginalOwners>,
    ready: tokio::sync::Notify,
    shutdown: CancellationToken,
}

impl WeixinCronBinding {
    pub(crate) fn new(shutdown: CancellationToken) -> Self {
        Self {
            shutdown,
            ..Self::default()
        }
    }

    pub(crate) fn bind(
        &self,
        state: &Arc<AppState>,
        channels: &Arc<ChannelRuntime>,
    ) -> Result<(), String> {
        let owners = OriginalOwners {
            state: Arc::downgrade(state),
            channels: Arc::downgrade(channels),
        };
        if let Some(existing) = self.owners.get() {
            return if existing.state.ptr_eq(&owners.state)
                && existing.channels.ptr_eq(&owners.channels)
            {
                Ok(())
            } else {
                Err("Weixin cron composition is already bound to another application".to_owned())
            };
        }
        self.owners
            .set(owners)
            .map_err(|_| "Weixin cron composition was already published".to_owned())?;
        self.ready.notify_waiters();
        Ok(())
    }

    async fn original_owners(&self) -> Result<(Arc<AppState>, Arc<ChannelRuntime>), String> {
        loop {
            // Register before inspecting OnceLock so publication between the
            // inspection and await cannot be missed by notify_waiters.
            let ready = self.ready.notified();
            tokio::pin!(ready);
            ready.as_mut().enable();
            if self.shutdown.is_cancelled() {
                return Err("Weixin cron scheduler is shutting down".to_owned());
            }
            if let Some(owners) = self.owners.get() {
                let state = owners
                    .state
                    .upgrade()
                    .ok_or_else(|| "Weixin cron application owner is unavailable".to_owned())?;
                let channels = owners
                    .channels
                    .upgrade()
                    .ok_or_else(|| "Weixin cron channel owner is unavailable".to_owned())?;
                return Ok((state, channels));
            }
            tokio::select! {
                biased;
                _ = self.shutdown.cancelled() => {
                    return Err("Weixin cron scheduler is shutting down".to_owned());
                }
                _ = ready => {}
            }
        }
    }
}

impl WeixinCronDelivery for WeixinCronBinding {
    fn freeze_target<'a>(
        &'a self,
        invocation: &'a SchedulerInvocation,
        workspace_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<WeixinDeliveryTarget>, String>> {
        Box::pin(async move {
            let (state, _channels) = self.original_owners().await?;
            let config = state.config.app_config.read().await;
            freeze_configured_target(&config.channels.weixin, invocation, workspace_id)
        })
    }

    fn send<'a>(
        &'a self,
        target: &'a WeixinDeliveryTarget,
        message_id: &'a str,
        text: &'a str,
    ) -> BoxFuture<'a, Result<WeixinSendOutcome, String>> {
        Box::pin(async move {
            let (state, channels) = self.original_owners().await?;
            let context = frozen_context(target)?;
            let config = state.config.app_config.read().await;
            validate_committed_account(&config.channels.weixin, target)?;
            let handle = channels
                .weixin_delivery_handle(&target.account_id, &target.credential_generation)?;
            // Keep this original config read guard until the producer's receipt:
            // a concurrent account commit cannot publish another generation
            // between the validation and the original network send effect.
            // The provider producer does not acquire AppState/config locks.
            let outcome = handle
                .send(
                    OutboundMessage::new(
                        "weixin",
                        target.channel_conversation.clone(),
                        ChatType::Direct,
                        text,
                    ),
                    &context,
                    message_id,
                )
                .await
                // The concrete producer returns Err only before HTTP admission;
                // any effectful receipt loss is its typed Unknown outcome.
                .map_err(|error| format!("Weixin local delivery was not admitted: {error}"));
            drop(config);
            outcome
        })
    }
}

fn freeze_configured_target(
    config: &WeixinChannelConfig,
    invocation: &SchedulerInvocation,
    workspace_id: &str,
) -> Result<Option<WeixinDeliveryTarget>, String> {
    // Only the occurrence's captured definition identity selects a setting.
    // A mutable current task, GUI workspace or another account is not a fallback.
    let Some(notification) = config.notifications.get(&invocation.task.definition_id) else {
        return Ok(None);
    };
    if invocation.task.definition_id.trim().is_empty()
        || notification.definition_id != invocation.task.definition_id
        || notification.workspace_id != workspace_id
        || workspace_id.trim().is_empty()
    {
        return Err(
            "Weixin notification does not match the occurrence definition/workspace".to_owned(),
        );
    }
    let target = WeixinDeliveryTarget {
        account_id: notification.account_id.clone(),
        credential_generation: notification.credential_generation.clone(),
        peer_id: notification.peer_id.clone(),
        context_ref: notification.context_ref.clone(),
        workspace_id: notification.workspace_id.clone(),
        channel_conversation: notification.channel_conversation.clone(),
        definition_id: notification.definition_id.clone(),
    };
    frozen_context(&target)?;
    // Being disconnected/disabled or replacing credentials cannot convert a
    // configured obligation into None or block the Agent task. The run freezes
    // the selected route; delivery reports its independent validation failure.
    Ok(Some(target))
}

fn validate_committed_account(
    config: &WeixinChannelConfig,
    target: &WeixinDeliveryTarget,
) -> Result<(), String> {
    if !config.enabled {
        return Err("Weixin notifications are disabled".to_owned());
    }
    let account = config
        .account
        .as_ref()
        .ok_or_else(|| "Weixin notification account is no longer configured".to_owned())?;
    if account.account_id != target.account_id
        || account.generation != target.credential_generation
        || target.account_id.trim().is_empty()
        || target.credential_generation.trim().is_empty()
    {
        return Err("Weixin notification belongs to another account authorization".to_owned());
    }
    Ok(())
}

fn frozen_context(target: &WeixinDeliveryTarget) -> Result<WeixinContextRef, String> {
    let context: WeixinContextRef = serde_json::from_str(&target.context_ref)
        .map_err(|_| "Weixin notification context reference is invalid".to_owned())?;
    if context.account_id != target.account_id
        || context.generation != target.credential_generation
        || context.peer != target.peer_id
        || context.reference.trim().is_empty()
    {
        return Err("Weixin notification context belongs to another destination".to_owned());
    }
    let canonical = encode_target(&target.account_id, &target.peer_id)
        .map_err(|_| "Weixin notification destination is invalid".to_owned())?;
    if target.channel_conversation != canonical {
        return Err("Weixin notification conversation belongs to another destination".to_owned());
    }
    Ok(context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{WeixinAccountConfig, WeixinNotificationConfig};
    use echo_agent::scheduler::{CronTask, SchedulerTrigger};

    fn configured() -> Result<(WeixinChannelConfig, SchedulerInvocation), Box<dyn std::error::Error>>
    {
        let mut task = CronTask::new("result", "* * * * *", "produce durable result");
        // SchedulerInvocation carries the store-issued captured definition;
        // CronTask::new intentionally has no persisted definition identity.
        task.definition_id = "captured-definition".to_owned();
        let context = WeixinContextRef {
            account_id: "account".to_owned(),
            generation: "authorization-1".to_owned(),
            peer: "peer".to_owned(),
            reference: "opaque-reference".to_owned(),
        };
        let notification = WeixinNotificationConfig {
            definition_id: task.definition_id.clone(),
            account_id: context.account_id.clone(),
            credential_generation: context.generation.clone(),
            peer_id: context.peer.clone(),
            context_ref: serde_json::to_string(&context)?,
            workspace_id: "occurrence-workspace".to_owned(),
            channel_conversation: encode_target(&context.account_id, &context.peer)?,
        };
        let mut config = WeixinChannelConfig {
            enabled: true,
            account: Some(WeixinAccountConfig {
                account_id: context.account_id,
                bot_token: "unused-offline-token".to_owned(),
                base_url: "https://ilinkai.weixin.qq.com".to_owned(),
                user_id: "peer".to_owned(),
                generation: context.generation,
            }),
            ..Default::default()
        };
        config
            .notifications
            .insert(task.definition_id.clone(), notification);
        Ok((
            config,
            SchedulerInvocation::new(
                "occurrence",
                task,
                chrono::Utc::now(),
                SchedulerTrigger::Manual,
            ),
        ))
    }

    #[tokio::test]
    async fn unpublished_composition_waits_until_original_scheduler_cancellation()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_, invocation) = configured()?;
        let shutdown = CancellationToken::new();
        let binding = WeixinCronBinding::new(shutdown.clone());
        let freeze = binding.freeze_target(&invocation, "occurrence-workspace");
        tokio::pin!(freeze);
        tokio::select! {
            biased;
            _ = &mut freeze => return Err("unpublished callback settled before binding or shutdown".into()),
            _ = tokio::task::yield_now() => {}
        }
        shutdown.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), freeze).await?;
        assert!(result.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn composition_publication_wakes_the_original_callback()
    -> Result<(), Box<dyn std::error::Error>> {
        let binding = WeixinCronBinding::default();
        let wait = binding.original_owners();
        tokio::pin!(wait);
        tokio::select! {
            biased;
            _ = &mut wait => return Err("unpublished owner wait settled".into()),
            _ = tokio::task::yield_now() => {}
        }
        binding
            .owners
            .set(OriginalOwners {
                state: Weak::new(),
                channels: Weak::new(),
            })
            .map_err(|_| "fixture already published")?;
        binding.ready.notify_waiters();
        // Dead original roots must fail after publication, never hang or be
        // replaced by a fresh/global/current application root.
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), wait).await?;
        assert!(result.is_err_and(|error| error.contains("application owner is unavailable")));
        Ok(())
    }

    #[test]
    fn exact_definition_and_occurrence_workspace_select_the_target()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut config, invocation) = configured()?;
        assert!(freeze_configured_target(&config, &invocation, "another-workspace").is_err());
        let target = freeze_configured_target(&config, &invocation, "occurrence-workspace")?
            .ok_or("target absent")?;
        assert_eq!(target.workspace_id, "occurrence-workspace");
        let notification = config
            .notifications
            .remove(&invocation.task.definition_id)
            .ok_or("setting absent")?;
        config
            .notifications
            .insert(invocation.task.id.clone(), notification);
        assert!(freeze_configured_target(&config, &invocation, "occurrence-workspace")?.is_none());
        Ok(())
    }

    #[test]
    fn later_account_or_context_cannot_retarget_the_frozen_destination()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut config, invocation) = configured()?;
        let target = freeze_configured_target(&config, &invocation, "occurrence-workspace")?
            .ok_or("target absent")?;
        let account = config.account.as_mut().ok_or("account absent")?;
        account.generation = "authorization-2".to_owned();
        assert!(validate_committed_account(&config, &target).is_err());
        let later_target = freeze_configured_target(&config, &invocation, "occurrence-workspace")?
            .ok_or("later obligation absent")?;
        assert_eq!(later_target, target);
        assert_eq!(target.credential_generation, "authorization-1");
        assert_eq!(frozen_context(&target)?.reference, "opaque-reference");
        let mut wrong_context = target.clone();
        let mut context = frozen_context(&target)?;
        context.peer = "another-peer".to_owned();
        wrong_context.context_ref = serde_json::to_string(&context)?;
        assert!(frozen_context(&wrong_context).is_err());
        let mut wrong_conversation = target;
        wrong_conversation.channel_conversation = encode_target("account", "another-peer")?;
        assert!(frozen_context(&wrong_conversation).is_err());
        Ok(())
    }

    #[test]
    fn disabled_delivery_remains_a_frozen_obligation() -> Result<(), Box<dyn std::error::Error>> {
        let (mut config, invocation) = configured()?;
        config.enabled = false;
        let target = freeze_configured_target(&config, &invocation, "occurrence-workspace")?
            .ok_or("target absent")?;
        assert!(validate_committed_account(&config, &target).is_err());
        assert!(frozen_context(&target).is_ok());
        Ok(())
    }
}

//! IM-only requests keep their direct provider. When a desktop observer exists,
//! the existing dispatcher owns one pair and arbitration across the two futures.
use super::{
    ChannelHumanLoopProvider, HitlDispatcher, HitlRequestScope, HitlScopeResolver,
    PairedHumanLoopProvider, PairedHumanLoopRequest,
};
use echo_agent::error::ReactError;
use echo_agent::human_loop::{HumanLoopProvider, HumanLoopRequest, HumanLoopResponse};
use futures::future::BoxFuture;
use std::sync::{Arc, Mutex};

pub type ChannelGuiHitlFactory = Arc<
    dyn Fn(
            HitlRequestScope,
            HitlScopeResolver,
        ) -> Result<Arc<dyn PairedHumanLoopProvider>, ReactError>
        + Send
        + Sync,
>;
pub type ChannelGuiHitlFactorySlot = Arc<Mutex<Option<ChannelGuiHitlFactory>>>;

pub struct ChannelHumanLoopEntry {
    channel: Arc<ChannelHumanLoopProvider>,
    gui: ChannelGuiHitlFactorySlot,
    scope: HitlScopeResolver,
}
impl ChannelHumanLoopEntry {
    pub fn new(
        channel: Arc<ChannelHumanLoopProvider>,
        gui: ChannelGuiHitlFactorySlot,
        scope: HitlScopeResolver,
    ) -> Self {
        Self {
            channel,
            gui,
            scope,
        }
    }
}
struct GuiPairedEntry {
    factory: ChannelGuiHitlFactory,
    scope: HitlScopeResolver,
}
impl PairedHumanLoopProvider for GuiPairedEntry {
    fn request_paired(
        &self,
        paired: PairedHumanLoopRequest,
    ) -> BoxFuture<'_, Result<HumanLoopResponse, ReactError>> {
        Box::pin(async move {
            paired.validate()?;
            let provider = (self.factory)(paired.identity.scope.clone(), self.scope.clone())?;
            provider.request_paired(paired).await
        })
    }
}
impl HumanLoopProvider for ChannelHumanLoopEntry {
    fn request(
        &self,
        request: HumanLoopRequest,
    ) -> BoxFuture<'_, Result<HumanLoopResponse, ReactError>> {
        Box::pin(async move {
            let factory = self
                .gui
                .lock()
                .map_err(|_| {
                    ReactError::Other("Original GUI HITL observer unavailable".to_string())
                })?
                .clone();
            let Some(factory) = factory else {
                return self.channel.request(request).await;
            };
            let dispatcher =
                Arc::new(HitlDispatcher::new().with_scope_resolver(self.scope.clone()));
            let _im = dispatcher
                .register_paired_owned("original IM request", self.channel.clone())
                .await;
            let _gui = dispatcher
                .register_paired_owned(
                    "original desktop observer",
                    Arc::new(GuiPairedEntry {
                        factory,
                        scope: self.scope.clone(),
                    }),
                )
                .await;
            dispatcher.request(request).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hitl::{ChannelHumanLoopResolution, HitlRequestIdentity};
    use echo_agent::human_loop::HumanLoopKind;
    use tokio::sync::{mpsc, oneshot};

    type GuiDelivery = (PairedHumanLoopRequest, oneshot::Sender<HumanLoopResponse>);
    struct GuiObserver(mpsc::UnboundedSender<GuiDelivery>);
    impl PairedHumanLoopProvider for GuiObserver {
        fn request_paired(
            &self,
            paired: PairedHumanLoopRequest,
        ) -> BoxFuture<'_, Result<HumanLoopResponse, ReactError>> {
            Box::pin(async move {
                paired.validate()?;
                let (answer, response) = oneshot::channel();
                self.0
                    .send((paired, answer))
                    .map_err(|_| ReactError::Other("GUI observer unavailable".into()))?;
                response
                    .await
                    .map_err(|error| ReactError::Other(error.to_string()))
            })
        }
    }

    #[tokio::test]
    async fn real_channel_entry_shares_one_pair_and_rejects_late_answers_after_either_surface_wins()
    -> anyhow::Result<()> {
        let scope = HitlRequestScope {
            workspace_id: Some("original-workspace".into()),
            conversation_id: Some("original-conversation".into()),
            channel_id: Some("qqbot".into()),
            sender_incarnation: Some("original-incarnation".into()),
            ..Default::default()
        };
        let observed_scope = scope.clone();
        let resolver: HitlScopeResolver = Arc::new(move || Ok(observed_scope.clone()));
        let channel =
            Arc::new(ChannelHumanLoopProvider::new().with_scope_resolver(resolver.clone()));
        let mut prompts = channel.subscribe_prompts();
        let (deliver, mut gui_deliveries) = mpsc::unbounded_channel();
        let observer = Arc::new(GuiObserver(deliver));
        let factory: ChannelGuiHitlFactory = Arc::new(move |_, _| Ok(observer.clone()));
        let entry = Arc::new(ChannelHumanLoopEntry::new(
            channel.clone(),
            Arc::new(Mutex::new(Some(factory))),
            resolver,
        ));
        let mut old_identity: Option<HitlRequestIdentity> = None;
        for kind in [
            HumanLoopKind::Approval,
            HumanLoopKind::Input,
            HumanLoopKind::Selection,
        ] {
            // IM first, GUI first, and both submissions in the same executor turn.
            for winner in 0..3 {
                let (mut request, expected_task_id, answer, response) = match kind {
                    HumanLoopKind::Approval => (
                        HumanLoopRequest::approval("read", serde_json::json!({})),
                        None,
                        "y",
                        HumanLoopResponse::Approved,
                    ),
                    HumanLoopKind::Input => (
                        HumanLoopRequest::input("中文输入🙂"),
                        None,
                        "中文\n🙂",
                        HumanLoopResponse::Text("中文\n🙂".into()),
                    ),
                    HumanLoopKind::Selection => (
                        HumanLoopRequest::selection("task", "choose", vec!["Continue".into()]),
                        Some("task"),
                        "1",
                        HumanLoopResponse::Selection {
                            selection: "Continue".into(),
                            instructions: None,
                        },
                    ),
                };
                // Expected scope comes from this fixture's inputs, not the
                // delivered pair: only Selection explicitly supplies a task.
                let expected_scope = HitlRequestScope {
                    task_id: expected_task_id.map(str::to_owned),
                    ..scope.clone()
                };
                request.request_id = Some("reused-original-id".into());
                request.timeout = Some(std::time::Duration::from_secs(2));
                let actor = entry.clone();
                let waiting = tokio::spawn(async move { actor.request(request).await });
                let (paired, gui_answer) =
                    tokio::time::timeout(std::time::Duration::from_secs(2), gui_deliveries.recv())
                        .await?
                        .ok_or_else(|| anyhow::anyhow!("GUI delivery missing"))?;
                let prompt =
                    tokio::time::timeout(std::time::Duration::from_secs(2), prompts.recv())
                        .await??;
                assert_eq!(paired.identity.scope, expected_scope);
                assert_eq!(paired.identity.request_id, "reused-original-id");
                assert!(prompt.contains(&paired.identity.response_target));
                if let Some(old) = old_identity.as_ref() {
                    assert_ne!(old.response_target, paired.identity.response_target);
                    assert!(matches!(
                        channel.try_resolve_message(&format!(
                            "/reply {} {answer}",
                            old.response_target
                        )),
                        ChannelHumanLoopResolution::Rejected(_)
                    ));
                }
                if winner != 1 {
                    assert!(matches!(
                        channel.try_resolve_message(&format!(
                            "/reply {} {answer}",
                            paired.identity.response_target
                        )),
                        ChannelHumanLoopResolution::Resolved(_)
                    ));
                }
                if winner != 0 {
                    gui_answer
                        .send(response)
                        .map_err(|_| anyhow::anyhow!("eligible GUI future disappeared"))?;
                }
                let result =
                    tokio::time::timeout(std::time::Duration::from_secs(2), waiting).await???;
                assert!(matches!(
                    (&kind, result),
                    (HumanLoopKind::Approval, HumanLoopResponse::Approved)
                        | (HumanLoopKind::Input, HumanLoopResponse::Text(_))
                        | (
                            HumanLoopKind::Selection,
                            HumanLoopResponse::Selection { .. }
                        )
                ));
                assert!(!matches!(
                    channel.try_resolve_message(&format!(
                        "/reply {} {answer}",
                        paired.identity.response_target
                    )),
                    ChannelHumanLoopResolution::Resolved(_)
                ));
                old_identity = Some(paired.identity);
                // A front-removal notification may announce no pending work.
                while prompts.try_recv().is_ok() {}
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn absent_desktop_factory_keeps_original_direct_im_provider() -> anyhow::Result<()> {
        let channel = Arc::new(ChannelHumanLoopProvider::new());
        let mut prompts = channel.subscribe_prompts();
        let entry = Arc::new(ChannelHumanLoopEntry::new(
            channel.clone(),
            Default::default(),
            Arc::new(|| Ok(HitlRequestScope::default())),
        ));
        let waiting =
            tokio::spawn(async move { entry.request(HumanLoopRequest::input("direct")).await });
        let prompt =
            tokio::time::timeout(std::time::Duration::from_secs(2), prompts.recv()).await??;
        let target = prompt
            .split_whitespace()
            .skip_while(|word| *word != "/reply")
            .nth(1)
            .ok_or_else(|| anyhow::anyhow!("target missing"))?;
        assert!(matches!(
            channel.try_resolve_message(&format!("/reply {target} original IM answer")),
            ChannelHumanLoopResolution::Resolved(_)
        ));
        assert!(
            matches!(waiting.await??, HumanLoopResponse::Text(text) if text == "original IM answer")
        );
        Ok(())
    }
}

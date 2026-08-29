fn authority_for(
    root: &Path,
    inboxes: &AgentInboxRegistry,
    target: &AgentAddress,
) -> Result<Arc<AgentInboxAuthority>, AgentRouterError> {
    {
        let _lifecycle = inboxes
            .lifecycle
            .lock()
            .map_err(|_| AgentRouterError::StateUnavailable)?;
        ensure_inbox_not_retiring(inboxes, target)?;
        if let Some(existing) = inboxes.authorities.get(target) {
            return Ok(Arc::clone(existing.value()));
        }
    }
    let opened = AgentInboxAuthority::open(root, target)?;
    let _lifecycle = inboxes
        .lifecycle
        .lock()
        .map_err(|_| AgentRouterError::StateUnavailable)?;
    ensure_inbox_not_retiring(inboxes, target)?;
    let entry = inboxes
        .authorities
        .entry(target.clone())
        .or_insert_with(|| Arc::clone(&opened));
    Ok(Arc::clone(entry.value()))
}

fn ensure_inbox_not_retiring(
    inboxes: &AgentInboxRegistry,
    target: &AgentAddress,
) -> Result<(), AgentRouterError> {
    if inboxes.retiring_targets.contains_key(target)
        || inboxes
            .retiring_workspaces
            .contains_key(&target.workspace_id)
    {
        Err(AgentRouterError::Retiring {
            workspace_id: target.workspace_id.to_string(),
            conversation_id: Some(target.conversation_id.clone()),
        })
    } else {
        Ok(())
    }
}

fn retire_target_sync(
    root: &Path,
    inboxes: &AgentInboxRegistry,
    target: &AgentAddress,
) -> Result<(), AgentRouterError> {
    if let Some((_, authority)) = inboxes.authorities.remove(target) {
        authority.close()?;
    }
    let path = inbox_dir(root, target);
    match std::fs::remove_dir_all(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(AgentRouterError::Io { path, source }),
    }
}

fn retire_workspace_sync(
    root: &Path,
    inboxes: &AgentInboxRegistry,
    workspace_id: &WorkspaceId,
) -> Result<(), AgentRouterError> {
    let targets = inboxes
        .authorities
        .iter()
        .filter(|entry| &entry.key().workspace_id == workspace_id)
        .map(|entry| entry.key().clone())
        .collect::<Vec<_>>();
    for target in targets {
        if let Some((_, authority)) = inboxes.authorities.remove(&target) {
            authority.close()?;
        }
    }
    let path = root
        .join("inboxes")
        .join(stable_segment(workspace_id.as_str()));
    match std::fs::remove_dir_all(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(AgentRouterError::Io { path, source }),
    }
}

fn enqueue_sync(
    root: &Path,
    inboxes: &AgentInboxRegistry,
    message: AgentMessage,
) -> Result<AgentDeliveryReceipt, AgentRouterError> {
    let target = message.to.clone();
    let authority = authority_for(root, inboxes, &target)?;
    let _operation = authority.lock_operation()?;
    let _lifecycle = inboxes
        .lifecycle
        .lock()
        .map_err(|_| AgentRouterError::StateUnavailable)?;
    ensure_inbox_not_retiring(inboxes, &target)?;
    let existing = authority
        .with_projection(|projection| Ok(projection.message(&message.message_id).cloned()))?;
    if let Some(existing) = existing {
        if !same_logical_message(&existing.message, &message) {
            return Err(AgentRouterError::IdCollision {
                message_id: message.message_id.clone(),
            });
        }
        return Ok(AgentDeliveryReceipt {
            message_id: existing.message.message_id.clone(),
            target: existing.message.to.clone(),
            phase: existing.phase,
            outcome: existing.outcome,
            drained: existing.drained,
            reason: existing.reason.clone(),
            persisted_at: existing.persisted_at,
            duplicate: true,
            durability: AgentDeliveryDurability::Unconfirmed,
        });
    }

    let persisted_at = Utc::now();
    let durability = authority.append(AgentInboxEvent::Persisted {
        message: message.clone(),
        persisted_at,
    })?;
    Ok(AgentDeliveryReceipt {
        message_id: message.message_id,
        target: message.to,
        phase: AgentDeliveryPhase::Persisted,
        outcome: None,
        drained: false,
        reason: None,
        persisted_at,
        duplicate: false,
        durability: durability.into(),
    })
}

fn same_logical_message(left: &AgentMessage, right: &AgentMessage) -> bool {
    left.message_id == right.message_id
        && left.from == right.from
        && left.to == right.to
        && left.payload == right.payload
        && left.correlation_id == right.correlation_id
        && left.causation_id == right.causation_id
        && left.origin == right.origin
}

fn pending_sync(
    root: &Path,
    inboxes: &AgentInboxRegistry,
    target: &AgentAddress,
) -> Result<Vec<AgentMessage>, AgentRouterError> {
    let authority = authority_for(root, inboxes, target)?;
    authority.with_projection(|projection| {
        Ok(projection
            .frontier_entries()
            .map(|entry| entry.message.clone())
            .collect())
    })
}

fn claim_next_sync(
    root: &Path,
    inboxes: &AgentInboxRegistry,
    target: &AgentAddress,
) -> Result<Option<AgentDeliveryClaim>, AgentRouterError> {
    let authority = authority_for(root, inboxes, target)?;
    let _operation = authority.lock_operation()?;
    let _lifecycle = inboxes
        .lifecycle
        .lock()
        .map_err(|_| AgentRouterError::StateUnavailable)?;
    ensure_inbox_not_retiring(inboxes, target)?;
    let next = authority.with_projection(|projection| Ok(projection.frontier_entry().cloned()))?;
    let Some(next) = next else {
        return Ok(None);
    };
    if next.effect_started_at.is_some()
        || matches!(
            next.phase,
            AgentDeliveryPhase::MailboxAccepted | AgentDeliveryPhase::Drained
        )
    {
        return Ok(None);
    }
    if next
        .next_attempt_at
        .is_some_and(|deadline| deadline > Utc::now())
    {
        return Ok(None);
    }
    let attempt = next.attempt.saturating_add(1);
    let attempt_id = uuid::Uuid::new_v4().to_string();
    let claimed_at = Utc::now();
    authority.append(AgentInboxEvent::Claimed {
        message_id: next.message.message_id.clone(),
        attempt_id: attempt_id.clone(),
        attempt,
        claimed_at,
    })?;
    Ok(Some(AgentDeliveryClaim {
        message: next.message,
        attempt_id,
        attempt,
        claimed_at,
    }))
}

fn in_flight_claim_sync(
    root: &Path,
    inboxes: &AgentInboxRegistry,
    target: &AgentAddress,
) -> Result<Option<AgentDeliveryInFlight>, AgentRouterError> {
    let authority = authority_for(root, inboxes, target)?;
    authority.with_projection(|projection| {
        let Some(entry) = projection.frontier_entry().cloned() else {
            return Ok(None);
        };
        if entry.effect_started_at.is_none()
            && !matches!(
                entry.phase,
                AgentDeliveryPhase::MailboxAccepted | AgentDeliveryPhase::Drained
            )
        {
            return Ok(None);
        }
        let attempt_id = entry.attempt_id.ok_or_else(|| {
            corrupt_event(
                &authority.directory,
                format!(
                    "injected message {} has no attempt identity",
                    entry.message.message_id
                ),
            )
        })?;
        let claimed_at = entry.claimed_at.ok_or_else(|| {
            corrupt_event(
                &authority.directory,
                format!(
                    "injected message {} has no claim timestamp",
                    entry.message.message_id
                ),
            )
        })?;
        let turn_id = entry.turn_id.ok_or_else(|| {
            corrupt_event(
                &authority.directory,
                "in-flight delivery has no turn identity".to_string(),
            )
        })?;
        Ok(Some(AgentDeliveryInFlight {
            claim: AgentDeliveryClaim {
                message: entry.message,
                attempt_id,
                attempt: entry.attempt,
                claimed_at,
            },
            phase: entry.phase,
            effect_started: entry.effect_started_at.is_some(),
            turn_id,
        }))
    })
}

fn settle_claim_sync(
    root: &Path,
    inboxes: &AgentInboxRegistry,
    claim: &AgentDeliveryClaim,
    settlement: ClaimSettlement,
) -> Result<AgentDeliveryReceipt, AgentRouterError> {
    let target = claim.message.to.clone();
    let authority = authority_for(root, inboxes, &target)?;
    let _operation = authority.lock_operation()?;
    let entry = authority.with_projection(|projection| {
        projection
            .message(&claim.message.message_id)
            .cloned()
            .ok_or_else(|| AgentRouterError::StaleClaim {
                message_id: claim.message.message_id.clone(),
                attempt_id: claim.attempt_id.clone(),
            })
    })?;
    let valid_phase = match &settlement {
        ClaimSettlement::EffectStarted { .. } => {
            entry.phase == AgentDeliveryPhase::Claimed && entry.effect_started_at.is_none()
        }
        ClaimSettlement::MailboxAccepted { turn_id } => {
            entry.phase == AgentDeliveryPhase::Claimed
                && entry.effect_started_at.is_some()
                && entry.turn_id.as_deref() == Some(turn_id)
        }
        ClaimSettlement::Drained { turn_id } => {
            entry.phase == AgentDeliveryPhase::MailboxAccepted
                && entry.turn_id.as_deref() == Some(turn_id)
        }
        ClaimSettlement::Deferred { .. } => matches!(entry.phase, AgentDeliveryPhase::Claimed),
        ClaimSettlement::TurnSettled {
            turn_id, drained, ..
        } => {
            matches!(
                entry.phase,
                AgentDeliveryPhase::Claimed
                    | AgentDeliveryPhase::MailboxAccepted
                    | AgentDeliveryPhase::Drained
            ) && turn_id
                .as_deref()
                .is_none_or(|turn_id| entry.turn_id.as_deref() == Some(turn_id))
                && drained.is_none_or(|drained| drained == entry.drained)
        }
    };
    if entry.attempt_id.as_deref() != Some(claim.attempt_id.as_str()) || !valid_phase {
        return Err(AgentRouterError::StaleClaim {
            message_id: claim.message.message_id.clone(),
            attempt_id: claim.attempt_id.clone(),
        });
    }
    let (phase, outcome, drained, reason, event) = match settlement {
        ClaimSettlement::EffectStarted { turn_id } => {
            let event = AgentInboxEvent::EffectStarted {
                message_id: claim.message.message_id.clone(),
                attempt_id: claim.attempt_id.clone(),
                started_at: Utc::now(),
                turn_id,
            };
            (AgentDeliveryPhase::Claimed, None, false, None, event)
        }
        ClaimSettlement::MailboxAccepted { turn_id } => {
            let event = AgentInboxEvent::MailboxAccepted {
                message_id: claim.message.message_id.clone(),
                attempt_id: claim.attempt_id.clone(),
                accepted_at: Utc::now(),
                turn_id,
            };
            (
                AgentDeliveryPhase::MailboxAccepted,
                None,
                false,
                None,
                event,
            )
        }
        ClaimSettlement::Drained { turn_id } => {
            let event = AgentInboxEvent::Drained {
                message_id: claim.message.message_id.clone(),
                attempt_id: claim.attempt_id.clone(),
                drained_at: Utc::now(),
                turn_id,
            };
            (AgentDeliveryPhase::Drained, None, true, None, event)
        }
        ClaimSettlement::Deferred {
            reason,
            next_attempt_at,
        } => {
            let event = AgentInboxEvent::Deferred {
                message_id: claim.message.message_id.clone(),
                attempt_id: claim.attempt_id.clone(),
                deferred_at: Utc::now(),
                reason: reason.clone(),
                next_attempt_at: Some(next_attempt_at),
            };
            (
                AgentDeliveryPhase::Persisted,
                None,
                false,
                Some(reason),
                event,
            )
        }
        ClaimSettlement::TurnSettled {
            turn_id,
            outcome,
            drained,
            reason,
            retryable,
            next_attempt_at,
            reply_message_id,
        } => {
            let drained = drained.unwrap_or(entry.drained);
            let turn_id = turn_id.or_else(|| entry.turn_id.clone());
            let event = AgentInboxEvent::TurnSettled {
                message_id: claim.message.message_id.clone(),
                attempt_id: claim.attempt_id.clone(),
                settled_at: Utc::now(),
                turn_id,
                outcome,
                drained,
                reason: reason.clone(),
                retryable,
                next_attempt_at,
                reply_message_id,
            };
            (
                AgentDeliveryPhase::TurnSettled,
                Some(outcome),
                drained,
                reason,
                event,
            )
        }
    };
    let persisted_at = entry.persisted_at;
    let durability = authority.append(event)?;
    Ok(AgentDeliveryReceipt {
        message_id: claim.message.message_id.clone(),
        target: target.clone(),
        phase,
        outcome,
        drained,
        reason,
        persisted_at,
        duplicate: false,
        durability: durability.into(),
    })
}

fn records_sync(
    root: &Path,
    inboxes: &AgentInboxRegistry,
    target: &AgentAddress,
) -> Result<Vec<AgentDeliveryRecord>, AgentRouterError> {
    let authority = authority_for(root, inboxes, target)?;
    authority.with_projection(|projection| {
        projection.validate(&authority.directory, target)?;
        Ok(projection
            .ordered(&authority.directory)?
            .into_iter()
            .map(FoldedDelivery::record)
            .collect())
    })
}

fn next_attempt_at_sync(
    root: &Path,
    inboxes: &AgentInboxRegistry,
    target: &AgentAddress,
) -> Result<Option<DateTime<Utc>>, AgentRouterError> {
    let authority = authority_for(root, inboxes, target)?;
    authority.with_projection(|projection| {
        Ok(projection
            .frontier_entry()
            .and_then(|entry| entry.next_attempt_at))
    })
}

fn retry_deadline(attempt: u32) -> DateTime<Utc> {
    let delay = RetryPolicy::default()
        .delay_for(attempt.max(1))
        .max(std::time::Duration::from_millis(100));
    let chrono_delay =
        chrono::Duration::from_std(delay).unwrap_or_else(|_| chrono::Duration::seconds(30));
    Utc::now() + chrono_delay
}

fn list_groups_sync(root: &Path) -> Result<Vec<AgentGroup>, AgentRouterError> {
    with_groups_lock(root, |groups_path| {
        let mut groups = read_groups(groups_path)?;
        groups.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.group_id.cmp(&right.group_id))
        });
        Ok(groups)
    })
}

fn create_group_sync(root: &Path, group: AgentGroup) -> Result<AgentGroup, AgentRouterError> {
    with_groups_lock(root, |groups_path| {
        let mut groups = read_groups(groups_path)?;
        if groups
            .iter()
            .any(|existing| existing.group_id == group.group_id)
        {
            return Err(AgentRouterError::IdCollision {
                message_id: group.group_id.clone(),
            });
        }
        groups.push(group.clone());
        write_groups(groups_path, &groups)?;
        Ok(group)
    })
}

fn update_group_sync(
    root: &Path,
    group_id: &str,
    name: String,
    leader: AgentAddress,
    members: Vec<AgentGroupMember>,
) -> Result<AgentGroup, AgentRouterError> {
    with_groups_lock(root, |groups_path| {
        let mut groups = read_groups(groups_path)?;
        let existing = groups
            .iter_mut()
            .find(|group| group.group_id == group_id)
            .ok_or_else(|| AgentRouterError::GroupNotFound(group_id.to_string()))?;
        let updated = AgentGroup {
            group_id: existing.group_id.clone(),
            name,
            leader,
            members,
            created_at: existing.created_at,
            updated_at: Utc::now(),
        };
        updated.validate()?;
        *existing = updated.clone();
        write_groups(groups_path, &groups)?;
        Ok(updated)
    })
}

fn delete_group_sync(root: &Path, group_id: &str) -> Result<bool, AgentRouterError> {
    with_groups_lock(root, |groups_path| {
        let mut groups = read_groups(groups_path)?;
        let before = groups.len();
        groups.retain(|group| group.group_id != group_id);
        if groups.len() == before {
            return Ok(false);
        }
        write_groups(groups_path, &groups)?;
        Ok(true)
    })
}

fn with_groups_lock<T>(
    root: &Path,
    operation: impl FnOnce(&Path) -> Result<T, AgentRouterError>,
) -> Result<T, AgentRouterError> {
    std::fs::create_dir_all(root).map_err(|source| AgentRouterError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    let lock_path = root.join("groups.lock");
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|source| AgentRouterError::Io {
            path: lock_path.clone(),
            source,
        })?;
    lock.lock_exclusive()
        .map_err(|source| AgentRouterError::Io {
            path: lock_path.clone(),
            source,
        })?;
    let result = operation(&root.join("groups.json"));
    let unlock = FileExt::unlock(&lock).map_err(|source| AgentRouterError::Io {
        path: lock_path,
        source,
    });
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

fn read_groups(path: &Path) -> Result<Vec<AgentGroup>, AgentRouterError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(AgentRouterError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let groups: Vec<AgentGroup> =
        serde_json::from_slice(&bytes).map_err(|error| AgentRouterError::Corrupt {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    for group in &groups {
        group.validate()?;
    }
    Ok(groups)
}

fn write_groups(path: &Path, groups: &[AgentGroup]) -> Result<(), AgentRouterError> {
    let encoded = serde_json::to_vec_pretty(groups).map_err(|error| AgentRouterError::Corrupt {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    echo_agent::utils::fs::atomic_write(path, &encoded).map_err(|source| AgentRouterError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FoldedDelivery {
    message: AgentMessage,
    persisted_at: DateTime<Utc>,
    phase: AgentDeliveryPhase,
    outcome: Option<AgentDeliveryOutcome>,
    drained: bool,
    reason: Option<String>,
    attempt_id: Option<String>,
    attempt: u32,
    claimed_at: Option<DateTime<Utc>>,
    effect_started_at: Option<DateTime<Utc>>,
    mailbox_accepted_at: Option<DateTime<Utc>>,
    drained_at: Option<DateTime<Utc>>,
    turn_settled_at: Option<DateTime<Utc>>,
    turn_id: Option<String>,
    reply_message_id: Option<String>,
    next_attempt_at: Option<DateTime<Utc>>,
    terminal: bool,
    retained_bytes: usize,
}

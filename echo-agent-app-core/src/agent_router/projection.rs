#[derive(Debug, Default, Serialize, Deserialize)]
struct AgentInboxProjection {
    order: VecDeque<String>,
    frontier: VecDeque<String>,
    entries: HashMap<String, FoldedDelivery>,
    terminal_retained_bytes: usize,
    invalid: Option<String>,
    #[cfg(test)]
    #[serde(skip)]
    full_validation_count: std::sync::atomic::AtomicUsize,
}

impl EventReducer for AgentInboxProjection {
    type Event = AgentInboxEvent;

    fn apply(&mut self, event: &Self::Event) {
        if self.invalid.is_none()
            && let Err(error) = self.apply_checked(event)
        {
            self.invalid = Some(error);
        }
    }
}

impl AgentInboxProjection {
    fn validate(&self, path: &Path, target: &AgentAddress) -> Result<(), AgentRouterError> {
        #[cfg(test)]
        self.full_validation_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.ensure_incremental_valid(path)?;
        let mut saw_non_terminal = false;
        let mut terminal_count = 0_usize;
        for message_id in &self.order {
            let entry = self.entries.get(message_id).ok_or_else(|| {
                corrupt_event(
                    path,
                    format!("message {message_id} disappeared from its projection"),
                )
            })?;
            if &entry.message.to != target {
                return Err(corrupt_event(
                    path,
                    format!("message {message_id} targets a different Agent address"),
                ));
            }
            if entry.terminal {
                if saw_non_terminal {
                    return Err(corrupt_event(
                        path,
                        "terminal Agent inbox entry appears after the live frontier".to_string(),
                    ));
                }
                terminal_count = terminal_count.saturating_add(1);
            } else {
                saw_non_terminal = true;
            }
        }
        if self.entries.len() != self.order.len() {
            return Err(corrupt_event(
                path,
                "Agent inbox projection order and entry counts differ".to_string(),
            ));
        }
        let mut frontier_ids = HashSet::with_capacity(self.frontier.len());
        for message_id in &self.frontier {
            if !frontier_ids.insert(message_id) {
                return Err(corrupt_event(
                    path,
                    format!("frontier contains duplicate message {message_id}"),
                ));
            }
            let entry = self.entries.get(message_id).ok_or_else(|| {
                corrupt_event(path, format!("frontier message {message_id} is missing"))
            })?;
            if entry.terminal {
                return Err(corrupt_event(
                    path,
                    format!("terminal message {message_id} remains on the frontier"),
                ));
            }
        }
        if self
            .entries
            .values()
            .filter(|entry| !entry.terminal)
            .count()
            != self.frontier.len()
        {
            return Err(corrupt_event(
                path,
                "Agent inbox frontier omits a non-terminal message".to_string(),
            ));
        }
        if terminal_count > INBOX_TERMINAL_RETENTION {
            return Err(corrupt_event(
                path,
                "Agent inbox terminal retention exceeds its fixed bound".to_string(),
            ));
        }
        let terminal_bytes = self
            .entries
            .values()
            .filter(|entry| entry.terminal)
            .map(|entry| entry.retained_bytes)
            .fold(0_usize, usize::saturating_add);
        if terminal_bytes != self.terminal_retained_bytes {
            return Err(corrupt_event(
                path,
                "Agent inbox terminal byte accounting diverged".to_string(),
            ));
        }
        if terminal_bytes > INBOX_TERMINAL_RETENTION_BYTES {
            return Err(corrupt_event(
                path,
                "Agent inbox terminal byte retention exceeds its fixed bound".to_string(),
            ));
        }
        Ok(())
    }

    fn ensure_incremental_valid(&self, path: &Path) -> Result<(), AgentRouterError> {
        match &self.invalid {
            Some(error) => Err(corrupt_event(path, error.clone())),
            None => Ok(()),
        }
    }

    fn message(&self, message_id: &str) -> Option<&FoldedDelivery> {
        self.entries.get(message_id)
    }

    fn frontier_entry(&self) -> Option<&FoldedDelivery> {
        self.frontier
            .front()
            .and_then(|message_id| self.entries.get(message_id))
    }

    fn frontier_entries(&self) -> impl Iterator<Item = &FoldedDelivery> {
        self.frontier
            .iter()
            .filter_map(|message_id| self.entries.get(message_id))
    }

    fn ordered(&self, path: &Path) -> Result<Vec<FoldedDelivery>, AgentRouterError> {
        self.order
            .iter()
            .map(|message_id| {
                self.entries.get(message_id).cloned().ok_or_else(|| {
                    corrupt_event(
                        path,
                        format!("message {message_id} disappeared from its projection"),
                    )
                })
            })
            .collect()
    }

    fn apply_checked(&mut self, event: &AgentInboxEvent) -> Result<(), String> {
        match event {
            AgentInboxEvent::Persisted {
                message,
                persisted_at,
            } => {
                if self.entries.contains_key(&message.message_id) {
                    return Err(format!("duplicate acceptance for {}", message.message_id));
                }
                self.order.push_back(message.message_id.clone());
                self.frontier.push_back(message.message_id.clone());
                self.entries.insert(
                    message.message_id.clone(),
                    FoldedDelivery {
                        message: message.clone(),
                        persisted_at: *persisted_at,
                        phase: AgentDeliveryPhase::Persisted,
                        outcome: None,
                        drained: false,
                        reason: None,
                        attempt_id: None,
                        attempt: 0,
                        claimed_at: None,
                        effect_started_at: None,
                        mailbox_accepted_at: None,
                        drained_at: None,
                        turn_settled_at: None,
                        turn_id: None,
                        reply_message_id: None,
                        next_attempt_at: None,
                        terminal: false,
                        retained_bytes: 0,
                    },
                );
            }
            AgentInboxEvent::Claimed {
                message_id,
                attempt_id,
                attempt,
                claimed_at,
            } => {
                let entry = projection_entry_mut(&mut self.entries, message_id)?;
                if entry.terminal {
                    return Err(format!("terminal message {message_id} was claimed again"));
                }
                entry.phase = AgentDeliveryPhase::Claimed;
                entry.outcome = None;
                entry.drained = false;
                entry.reason = None;
                entry.attempt_id = Some(attempt_id.clone());
                entry.attempt = *attempt;
                entry.claimed_at = Some(*claimed_at);
                entry.effect_started_at = None;
                entry.mailbox_accepted_at = None;
                entry.drained_at = None;
                entry.turn_settled_at = None;
                entry.turn_id = None;
                entry.reply_message_id = None;
                entry.next_attempt_at = None;
            }
            AgentInboxEvent::EffectStarted {
                message_id,
                attempt_id,
                started_at,
                turn_id,
            } => {
                let entry =
                    projection_claimed_entry_mut(&mut self.entries, message_id, attempt_id)?;
                if entry.phase != AgentDeliveryPhase::Claimed || entry.effect_started_at.is_some() {
                    return Err(format!(
                        "delivery injection was started twice for {message_id}"
                    ));
                }
                entry.effect_started_at = Some(*started_at);
                entry.turn_id = Some(turn_id.clone());
            }
            AgentInboxEvent::MailboxAccepted {
                message_id,
                attempt_id,
                accepted_at,
                turn_id,
            } => {
                let entry =
                    projection_claimed_entry_mut(&mut self.entries, message_id, attempt_id)?;
                if entry.phase != AgentDeliveryPhase::Claimed || entry.effect_started_at.is_none() {
                    return Err(format!(
                        "delivery mailbox accepted without an effect-started fact for {message_id}"
                    ));
                }
                if entry.turn_id.as_deref() != Some(turn_id) {
                    return Err(format!(
                        "delivery mailbox-accepted turn changed for {message_id}"
                    ));
                }
                entry.phase = AgentDeliveryPhase::MailboxAccepted;
                entry.mailbox_accepted_at = Some(*accepted_at);
            }
            AgentInboxEvent::Drained {
                message_id,
                attempt_id,
                drained_at,
                turn_id,
            } => {
                let entry =
                    projection_claimed_entry_mut(&mut self.entries, message_id, attempt_id)?;
                if entry.phase != AgentDeliveryPhase::MailboxAccepted {
                    return Err(format!(
                        "delivery drained without mailbox acceptance for {message_id}"
                    ));
                }
                if entry.turn_id.as_deref() != Some(turn_id) {
                    return Err(format!("delivery drained turn changed for {message_id}"));
                }
                entry.phase = AgentDeliveryPhase::Drained;
                entry.drained = true;
                entry.drained_at = Some(*drained_at);
            }
            AgentInboxEvent::Deferred {
                message_id,
                attempt_id,
                deferred_at: _deferred_at,
                reason,
                next_attempt_at,
            } => {
                let entry =
                    projection_claimed_entry_mut(&mut self.entries, message_id, attempt_id)?;
                entry.phase = AgentDeliveryPhase::Persisted;
                entry.outcome = None;
                entry.drained = false;
                entry.reason = Some(reason.clone());
                entry.effect_started_at = None;
                entry.mailbox_accepted_at = None;
                entry.drained_at = None;
                entry.turn_settled_at = None;
                entry.turn_id = None;
                entry.next_attempt_at = *next_attempt_at;
            }
            AgentInboxEvent::TurnSettled {
                message_id,
                attempt_id,
                settled_at,
                turn_id,
                outcome,
                drained,
                reason,
                retryable,
                next_attempt_at,
                reply_message_id,
            } => {
                let terminal = *drained || !retryable;
                {
                    let entry =
                        projection_claimed_entry_mut(&mut self.entries, message_id, attempt_id)?;
                    if *drained != entry.drained {
                        return Err(format!(
                            "delivery terminal drain flag changed for {message_id}"
                        ));
                    }
                    if let Some(turn_id) = turn_id
                        && entry
                            .turn_id
                            .as_deref()
                            .is_some_and(|current| current != turn_id)
                    {
                        return Err(format!("delivery terminal turn changed for {message_id}"));
                    }
                    entry.phase = AgentDeliveryPhase::TurnSettled;
                    entry.outcome = Some(*outcome);
                    entry.reason = reason.clone();
                    entry.turn_settled_at = Some(*settled_at);
                    if turn_id.is_some() {
                        entry.turn_id = turn_id.clone();
                    }
                    entry.reply_message_id = reply_message_id.clone();
                    entry.terminal = terminal;
                    entry.next_attempt_at = *next_attempt_at;
                }
                if terminal {
                    self.retain_terminal(message_id)?;
                }
            }
        }
        Ok(())
    }

    fn retire_frontier(&mut self, message_id: &str) -> Result<(), String> {
        if self.frontier.front().map(String::as_str) != Some(message_id) {
            return Err(format!(
                "terminal delivery {message_id} is not the FIFO frontier owner"
            ));
        }
        self.frontier.pop_front();
        Ok(())
    }

    fn retain_terminal(&mut self, message_id: &str) -> Result<(), String> {
        let retained_bytes = {
            let entry = self
                .entries
                .get_mut(message_id)
                .ok_or_else(|| format!("terminal message {message_id} is missing"))?;
            let payload = serde_json::to_vec(&entry)
                .map_err(|error| format!("terminal retention encoding failed: {error}"))?;
            let retained_bytes = payload
                .len()
                .saturating_add(message_id.len().saturating_mul(3))
                .saturating_add(128);
            entry.retained_bytes = retained_bytes;
            retained_bytes
        };
        self.terminal_retained_bytes = self.terminal_retained_bytes.saturating_add(retained_bytes);
        self.retire_frontier(message_id)?;
        self.trim_terminal_history()
    }

    fn trim_terminal_history(&mut self) -> Result<(), String> {
        while self.entries.len().saturating_sub(self.frontier.len()) > INBOX_TERMINAL_RETENTION
            || self.terminal_retained_bytes > INBOX_TERMINAL_RETENTION_BYTES
        {
            let message_id = self
                .order
                .pop_front()
                .ok_or_else(|| "Agent inbox terminal retention lost its order".to_string())?;
            let terminal = self
                .entries
                .get(&message_id)
                .is_some_and(|entry| entry.terminal);
            if !terminal {
                return Err(format!(
                    "Agent inbox attempted to evict live frontier message {message_id}"
                ));
            }
            let removed = self
                .entries
                .remove(&message_id)
                .ok_or_else(|| format!("terminal message {message_id} disappeared during trim"))?;
            self.terminal_retained_bytes = self
                .terminal_retained_bytes
                .saturating_sub(removed.retained_bytes);
        }
        Ok(())
    }
}

impl FoldedDelivery {
    fn record(self) -> AgentDeliveryRecord {
        let message = self.message;
        AgentDeliveryRecord {
            message_id: message.message_id.clone(),
            target: message.to.clone(),
            message,
            phase: self.phase,
            outcome: self.outcome,
            drained: self.drained,
            reason: self.reason,
            persisted_at: self.persisted_at,
            attempt_id: self.attempt_id,
            attempt: self.attempt,
            claimed_at: self.claimed_at,
            mailbox_accepted_at: self.mailbox_accepted_at,
            drained_at: self.drained_at,
            turn_settled_at: self.turn_settled_at,
            turn_id: self.turn_id,
            reply_message_id: self.reply_message_id,
            next_attempt_at: self.next_attempt_at,
        }
    }
}

fn projection_entry_mut<'a>(
    entries: &'a mut HashMap<String, FoldedDelivery>,
    message_id: &str,
) -> Result<&'a mut FoldedDelivery, String> {
    entries
        .get_mut(message_id)
        .ok_or_else(|| format!("delivery event references unknown message {message_id}"))
}

fn projection_claimed_entry_mut<'a>(
    entries: &'a mut HashMap<String, FoldedDelivery>,
    message_id: &str,
    attempt_id: &str,
) -> Result<&'a mut FoldedDelivery, String> {
    let entry = projection_entry_mut(entries, message_id)?;
    if !matches!(
        entry.phase,
        AgentDeliveryPhase::Claimed
            | AgentDeliveryPhase::MailboxAccepted
            | AgentDeliveryPhase::Drained
            | AgentDeliveryPhase::TurnSettled
    ) || entry.terminal
        || entry.attempt_id.as_deref() != Some(attempt_id)
    {
        return Err(format!(
            "delivery event has stale claim {attempt_id} for {message_id}"
        ));
    }
    Ok(entry)
}

fn corrupt_event(path: &Path, message: String) -> AgentRouterError {
    AgentRouterError::Corrupt {
        path: path.to_path_buf(),
        message,
    }
}

fn inbox_dir(root: &Path, target: &AgentAddress) -> PathBuf {
    root.join("inboxes")
        .join(stable_segment(target.workspace_id.as_str()))
        .join(stable_segment(&target.conversation_id))
}

fn stable_segment(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn journal_error(path: &Path, error: echo_agent::error::ReactError) -> AgentRouterError {
    AgentRouterError::Corrupt {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

// Process-level retry owner for plugin cleanup receipts.
//
// A failed `PluginRuntimeService` bootstrap has no live runtime to adopt the
// obligations it still owns, and those receipts hold real resources: exact MCP
// name ownership tokens, detached lifecycle managers, plugin monitors and
// pending uninstalls. Turning them into log lines would hand those obligations
// to nobody, so they are parked here instead — a live, process-scoped owner that
// the next bootstrap for the same project root adopts and retries.
//
// Lifecycle of a parked receipt:
//   1. `retain_cleanup_receipts` parks it (bootstrap failure, or a runtime that
//      was dropped while still unsettled).
//   2. `take_parked_cleanup_receipts` moves every receipt for one root into the
//      live runtime's ledger; the runtime is now the owner.
//   3. `PluginRuntimeService::retry_cleanup_receipts` settles it in place on
//      every mutation and removes it only after the exact obligation settled.
//
// Parking is therefore never a disposal: a receipt leaves this ledger only by
// moving into a live runtime's ledger, and it leaves that ledger only after a
// confirmed settlement (or by being parked again when its runtime is dropped).

use std::sync::OnceLock;

/// Receipts whose owning runtime no longer exists, keyed by project root.
fn parked_receipts() -> &'static std::sync::Mutex<Vec<CleanupReceipt>> {
    static PARKED: OnceLock<std::sync::Mutex<Vec<CleanupReceipt>>> = OnceLock::new();
    PARKED.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

fn lock_parked_receipts() -> std::sync::MutexGuard<'static, Vec<CleanupReceipt>> {
    // A panicking holder must not turn every later bootstrap into a panic; the
    // ledger is a plain Vec with no cross-entry invariant to preserve.
    parked_receipts()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Park receipts with the process-level retry owner and report them.
///
/// The returned strings are diagnostics for logs and error context; the
/// obligations themselves now live in the ledger, not in those strings.
pub(crate) fn retain_cleanup_receipts(receipts: Vec<CleanupReceipt>) -> Vec<String> {
    if receipts.is_empty() {
        return Vec::new();
    }
    let mut descriptions = Vec::with_capacity(receipts.len());
    let mut parked = lock_parked_receipts();
    for receipt in receipts {
        descriptions.push(receipt.describe());
        parked.push(receipt);
    }
    descriptions
}

/// Move every parked receipt for one project root into the caller's ledger.
///
/// Receipts for other roots stay parked. The root selects obligations to retry;
/// each exported receipt retains its original target, registry and scheduler,
/// so lookup never transfers authority to the replacement runtime.
pub(crate) fn take_parked_cleanup_receipts(root: &Path) -> Vec<CleanupReceipt> {
    let mut parked = lock_parked_receipts();
    let mut taken = Vec::new();
    let mut remaining = Vec::with_capacity(parked.len());
    for receipt in parked.drain(..) {
        if receipt.root() == root {
            taken.push(receipt);
        } else {
            remaining.push(receipt);
        }
    }
    *parked = remaining;
    taken
}

/// Project roots that still owe a process-level plugin cleanup retry.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn parked_cleanup_debt_roots() -> Vec<PathBuf> {
    let mut roots = lock_parked_receipts()
        .iter()
        .map(|receipt| receipt.root().to_path_buf())
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    roots
}

#[cfg(test)]
pub(crate) fn parked_cleanup_receipts_for_root(root: &Path) -> Vec<CleanupReceipt> {
    lock_parked_receipts()
        .iter()
        .filter(|receipt| receipt.root() == root)
        .cloned()
        .collect()
}

/// A receipt collection that can only leave scope parked.
///
/// Bootstrap failures hand their receipts to the caller as this type. The caller
/// either parks them explicitly with [`Self::park`] or drops them; both end in
/// the process-level ledger, so a bootstrap failure can never silently forget an
/// obligation.
#[derive(Debug)]
pub(crate) struct RetainedCleanupReceipts {
    receipts: Vec<CleanupReceipt>,
}

impl RetainedCleanupReceipts {
    pub(crate) fn new(receipts: Vec<CleanupReceipt>) -> Self {
        Self { receipts }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn is_empty(&self) -> bool {
        self.receipts.is_empty()
    }

    pub(crate) fn len(&self) -> usize {
        self.receipts.len()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn iter(&self) -> impl Iterator<Item = &CleanupReceipt> {
        self.receipts.iter()
    }

    pub(crate) fn describe(&self) -> Vec<String> {
        self.receipts.iter().map(CleanupReceipt::describe).collect()
    }

    /// Exact `plugin:name=token` triples for every retained claim.
    pub(crate) fn mcp_tokens(&self) -> Vec<String> {
        self.receipts
            .iter()
            .flat_map(|receipt| receipt.mcp_claims().iter())
            .map(|claim| format!("{}:{}={:?}", claim.plugin_id(), claim.name(), claim.token()))
            .collect()
    }

    /// Hand every receipt to the process-level retry owner.
    pub(crate) fn park(mut self) -> Vec<String> {
        retain_cleanup_receipts(std::mem::take(&mut self.receipts))
    }
}

impl Drop for RetainedCleanupReceipts {
    fn drop(&mut self) {
        if self.receipts.is_empty() {
            return;
        }
        let descriptions = retain_cleanup_receipts(std::mem::take(&mut self.receipts));
        tracing::error!(
            receipts = descriptions.len(),
            "plugin cleanup receipts left scope without an owner; parked them in the process retry ledger"
        );
    }
}

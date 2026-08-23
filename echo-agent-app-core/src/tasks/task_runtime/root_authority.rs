//! Process-canonical authority for EKO TaskRuntime directory transactions.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock, Weak};

use echo_agent::utils::fs::{ExclusiveFileLease, create_dir_all_durable, try_exclusive_file_lease};

use super::file_shadow::ShadowError;

struct RootInner {
    root: PathBuf,
    operation: RwLock<()>,
    lease: Mutex<Option<ExclusiveFileLease>>,
}

pub(crate) struct RootTransactionAuthority {
    inner: Arc<RootInner>,
    slot: Arc<RootSlot>,
}

enum RootSlotStatus {
    Opening,
    Ready {
        inner: Weak<RootInner>,
        handles: usize,
    },
    Closing,
    Vacant,
}

struct RootSlot {
    status: Mutex<RootSlotStatus>,
    changed: Condvar,
}

#[derive(Default)]
struct RootRegistry {
    entries: HashMap<PathBuf, Arc<RootSlot>>,
    operations: usize,
}

fn root_registry() -> &'static Mutex<RootRegistry> {
    static REGISTRY: OnceLock<Mutex<RootRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(RootRegistry::default()))
}

fn prune_registry(registry: &mut RootRegistry) {
    registry.operations = registry.operations.saturating_add(1);
    if !registry.operations.is_multiple_of(32) && registry.entries.len() <= 256 {
        return;
    }
    registry.entries.retain(|_, slot| {
        if Arc::strong_count(slot) > 1 {
            return true;
        }
        let status = slot
            .status
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        !matches!(*status, RootSlotStatus::Vacant)
    });
}

fn canonical_root_key(root: &Path) -> Result<PathBuf, ShadowError> {
    let parent = root
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    create_dir_all_durable(parent).map_err(|error| ShadowError::Io(error.to_string()))?;
    let parent =
        std::fs::canonicalize(parent).map_err(|error| ShadowError::Io(error.to_string()))?;
    let name = root
        .file_name()
        .ok_or_else(|| ShadowError::Io("TaskRuntime root has no final component".to_string()))?;
    let candidate = parent.join(name);
    match std::fs::symlink_metadata(&candidate) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(ShadowError::Io(format!(
            "TaskRuntime root must not be a symlink: {}",
            candidate.display()
        ))),
        Ok(metadata) if !metadata.is_dir() => Err(ShadowError::Io(format!(
            "TaskRuntime root is not a directory: {}",
            candidate.display()
        ))),
        Ok(_) => {
            std::fs::canonicalize(&candidate).map_err(|error| ShadowError::Io(error.to_string()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(candidate),
        Err(error) => Err(ShadowError::Io(error.to_string())),
    }
}

#[cfg(test)]
type RootDropPause = Arc<(std::sync::Barrier, std::sync::Barrier)>;

#[cfg(test)]
fn drop_pauses() -> &'static Mutex<HashMap<PathBuf, RootDropPause>> {
    static PAUSES: OnceLock<Mutex<HashMap<PathBuf, RootDropPause>>> = OnceLock::new();
    PAUSES.get_or_init(|| Mutex::new(HashMap::new()))
}

impl Clone for RootTransactionAuthority {
    fn clone(&self) -> Self {
        let mut status = self
            .slot
            .status
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let RootSlotStatus::Ready { inner, handles } = &mut *status
            && inner
                .upgrade()
                .is_some_and(|candidate| Arc::ptr_eq(&candidate, &self.inner))
        {
            *handles = handles.saturating_add(1);
        }
        Self {
            inner: Arc::clone(&self.inner),
            slot: Arc::clone(&self.slot),
        }
    }
}

impl Drop for RootTransactionAuthority {
    fn drop(&mut self) {
        let last = {
            let mut status = self
                .slot
                .status
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match &mut *status {
                RootSlotStatus::Ready { inner, handles }
                    if inner
                        .upgrade()
                        .is_some_and(|candidate| Arc::ptr_eq(&candidate, &self.inner)) =>
                {
                    *handles = handles.saturating_sub(1);
                    if *handles == 0 {
                        *status = RootSlotStatus::Closing;
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            }
        };
        if !last {
            return;
        }
        #[cfg(test)]
        let pause = drop_pauses()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.inner.root);
        #[cfg(test)]
        if let Some(pause) = pause {
            pause.0.wait();
            pause.1.wait();
        }
        drop(
            self.inner
                .lease
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take(),
        );
        let mut status = self
            .slot
            .status
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if matches!(*status, RootSlotStatus::Closing) {
            *status = RootSlotStatus::Vacant;
        }
        self.slot.changed.notify_all();
    }
}

impl RootTransactionAuthority {
    #[cfg(test)]
    pub(crate) fn pause_next_drop_for_test(
        root: &Path,
    ) -> Result<Arc<(std::sync::Barrier, std::sync::Barrier)>, ShadowError> {
        let root = canonical_root_key(root)?;
        let pause = Arc::new((std::sync::Barrier::new(2), std::sync::Barrier::new(2)));
        drop_pauses()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(root, Arc::clone(&pause));
        Ok(pause)
    }

    #[cfg(test)]
    pub(crate) fn held_lookup_survives_prune_for_test(root: &Path) -> Result<bool, ShadowError> {
        let authority = Self::open(root)?;
        let key = authority.inner.root.clone();
        drop(authority);
        let held = {
            let registry = root_registry().lock().map_err(|error| {
                ShadowError::Io(format!("TaskRuntime root registry poisoned: {error}"))
            })?;
            registry.entries.get(&key).cloned()
        }
        .ok_or_else(|| ShadowError::Io("root registry slot missing".to_string()))?;
        let retained = {
            let mut registry = root_registry().lock().map_err(|error| {
                ShadowError::Io(format!("TaskRuntime root registry poisoned: {error}"))
            })?;
            registry.operations = 31;
            prune_registry(&mut registry);
            registry
                .entries
                .get(&key)
                .is_some_and(|slot| Arc::ptr_eq(slot, &held))
        };
        Ok(retained)
    }

    pub(crate) fn open(root: &Path) -> Result<Self, ShadowError> {
        let root = canonical_root_key(root)?;
        let (slot, mut opener) = {
            let mut registry = root_registry().lock().map_err(|error| {
                ShadowError::Io(format!("TaskRuntime root registry poisoned: {error}"))
            })?;
            prune_registry(&mut registry);
            match registry.entries.get(&root) {
                Some(slot) => (Arc::clone(slot), false),
                None => {
                    let slot = Arc::new(RootSlot {
                        status: Mutex::new(RootSlotStatus::Opening),
                        changed: Condvar::new(),
                    });
                    registry.entries.insert(root.clone(), Arc::clone(&slot));
                    (slot, true)
                }
            }
        };
        loop {
            if opener {
                let opened = try_exclusive_file_lease(&root).map_err(|error| {
                    ShadowError::Io(format!(
                        "TaskRuntime root {} is owned by another process: {error}",
                        root.display()
                    ))
                });
                let mut status = slot
                    .status
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                match opened {
                    Ok(lease) => {
                        let inner = Arc::new(RootInner {
                            root: root.clone(),
                            operation: RwLock::new(()),
                            lease: Mutex::new(Some(lease)),
                        });
                        *status = RootSlotStatus::Ready {
                            inner: Arc::downgrade(&inner),
                            handles: 1,
                        };
                        slot.changed.notify_all();
                        return Ok(Self {
                            inner,
                            slot: Arc::clone(&slot),
                        });
                    }
                    Err(error) => {
                        *status = RootSlotStatus::Vacant;
                        slot.changed.notify_all();
                        return Err(error);
                    }
                }
            }
            let mut status = slot
                .status
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match &mut *status {
                RootSlotStatus::Opening | RootSlotStatus::Closing => {
                    status = slot
                        .changed
                        .wait(status)
                        .unwrap_or_else(|error| error.into_inner());
                    drop(status);
                }
                RootSlotStatus::Vacant => {
                    *status = RootSlotStatus::Opening;
                    opener = true;
                }
                RootSlotStatus::Ready { inner, handles } => {
                    if let Some(inner) = inner.upgrade() {
                        *handles = handles.saturating_add(1);
                        return Ok(Self {
                            inner,
                            slot: Arc::clone(&slot),
                        });
                    }
                    *status = RootSlotStatus::Opening;
                    opener = true;
                }
            }
        }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.inner.root
    }

    pub(crate) fn same_authority(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    pub(crate) fn read_operation(&self) -> std::sync::RwLockReadGuard<'_, ()> {
        self.inner
            .operation
            .read()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub(crate) fn write_operation(&self) -> std::sync::RwLockWriteGuard<'_, ()> {
        self.inner
            .operation
            .write()
            .unwrap_or_else(|error| error.into_inner())
    }
}

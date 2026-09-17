//! Optional phase-level progress observation for lifecycle operations.
//!
//! A consumer such as the CLI installs one process-wide observer with
//! [`set_progress_observer`] before invoking lifecycle operations. Events are
//! emitted only at real, durable state transitions and at lock-contention
//! boundaries — never on timers, per-file work, or estimated percentages — so
//! the number of events is bounded by the number of journaled transitions an
//! operation performs. When no observer is installed the emission sites cost
//! one atomic load and lifecycle behaviour is unchanged.

use std::sync::OnceLock;

use crate::{AddWorktreePhase, GarbageCollectionPhase};

/// One observed lifecycle progress event.
///
/// Every variant reflects a state the operation genuinely reached; there is
/// no interpolation between events.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProgressEvent {
    /// A coordination lock is held by another process; the current operation
    /// is now waiting for it. Emitted once per contended acquisition.
    LockContended {
        /// The lock operation being waited on, e.g. `lock immutable base`.
        operation: &'static str,
    },
    /// A previously contended coordination lock has been acquired and the
    /// operation is proceeding again.
    LockAcquired {
        /// The lock operation that was waited on.
        operation: &'static str,
    },
    /// A journaled worktree-add operation durably reached `phase`.
    AddPhase {
        /// The phase recorded in the add journal.
        phase: AddWorktreePhase,
    },
    /// The add found a completed immutable base and reuses it (cached path).
    BaseReused,
    /// The add is materializing a new immutable base from Git (cold path).
    BaseMaterializing,
    /// Repair inventoried this many journaled operations to examine.
    RepairScanned {
        /// Total journaled operations across all lifecycle journal stores.
        operations: usize,
    },
    /// Repair is resuming or rolling back one journaled operation.
    RepairRecovering {
        /// The journal kind, e.g. `add`, `removal`, `move`, `compaction`,
        /// `prune`, or `garbage-collection`.
        kind: &'static str,
        /// The journaled operation identifier.
        operation_id: String,
    },
    /// Garbage collection planned this many unreferenced candidate bases.
    GcPlanned {
        /// Number of collectible bases in the plan.
        candidates: usize,
    },
    /// A garbage-collection journal durably reached `phase`.
    GcPhase {
        /// The phase recorded in the collection journal.
        phase: GarbageCollectionPhase,
    },
}

/// The observer callback type accepted by [`set_progress_observer`].
pub type ProgressObserver = dyn Fn(&ProgressEvent) + Send + Sync;

static OBSERVER: OnceLock<Box<ProgressObserver>> = OnceLock::new();

/// Install a process-wide progress observer.
///
/// The observer is optional: lifecycle operations behave identically with or
/// without one. At most one observer can be installed per process; the first
/// call wins and later calls return `false` without replacing it. The
/// observer runs on the thread performing the operation, so it should return
/// quickly and must not call back into lifecycle operations.
pub fn set_progress_observer(observer: Box<ProgressObserver>) -> bool {
    OBSERVER.set(observer).is_ok()
}

/// Deliver one event to the installed observer, if any.
pub(crate) fn emit(event: ProgressEvent) {
    if let Some(observer) = OBSERVER.get() {
        observer(&event);
    }
}

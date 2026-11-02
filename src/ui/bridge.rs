//! Job routing and row identity for the workspace window.
//!
//! STUB for the behavioral RED pass: the types and verbs below are the contract the
//! tests are written against; the routing decisions are not implemented yet.

use crate::application::jobs::{
    CancelToken, Delivered, JobCompletion, JobHost, JobId, JobKind, JobOwner, JobProgress,
    JobRequest, JobSource, JobWork,
};
use std::sync::Arc;

/// Hands the job host a coalesced notice that a result or a report is waiting.
///
/// This is the window's side of the host's private notification seam: `on_wake` runs on a
/// worker thread with no host lock held, so it must do nothing but schedule work onto the UI
/// thread — for a Slint window, `slint::invoke_from_event_loop`. The install happens once:
/// a second attempt is refused with `false` rather than swapping a callback a live drain may
/// be using, and `false` is also what a caller takes when there is nothing pending.
#[must_use]
pub fn install_wakeup(host: &JobHost, on_wake: impl Fn() + Send + Sync + 'static) -> bool {
    host.install_wakeup(Arc::new(on_wake))
}

/// Takes the pending notice and then drains everything the host has for `current`.
///
/// Taking first is the whole of the no-lost-wake argument: a result posted during the drain
/// re-arms the notice, so the next wake finds it, while a notice that was set before this
/// call can never be the reason a queued result goes unseen. The returned vector is what the
/// window maps onto its panes; nothing here touches a Slint type, so the ordering itself is
/// testable.
#[must_use]
pub fn drain(host: &JobHost, current: Option<JobSource>) -> Vec<Delivered> {
    let _wake = host.take_wakeup();
    let mut arrived = Vec::new();
    while let Some(delivered) = host.poll_completion(current) {
        arrived.push(delivered);
    }
    arrived
}
use crate::application::workflow::WorkflowEffect;
use crate::domain::types::{ModId, ModMetadata};
use std::sync::atomic::{AtomicU64, Ordering};

/// Which slot of the workspace a background computation belongs to.
///
/// Slots are per kind of work, not one global "last job": a refresh that was
/// superseded must not be able to install, while an unrelated sort that is still
/// the active one for its own slot is unaffected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JobSlot {
    /// Re-reading an installation, whether the bootstrap read or a later refresh.
    Refresh,
    /// Putting the working list into load order.
    Sort,
    /// Writing the configuration.
    Save,
    /// Starting the game.
    Launch,
}

/// The answer [`ComputeJobs::route`] gives the adapter about one completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Routed {
    /// This result belongs to the job the slot is still waiting for: act on it.
    Accepted(JobKind),
    /// A result for a job this slot no longer has: superseded, cancelled or never
    /// dispatched here. It is dropped and changes nothing, even when its session
    /// binding still matches.
    Superseded,
}

/// The job a slot is already waiting for, which refused to make room for another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Busy {
    pub slot: JobSlot,
    pub job: JobId,
}

impl JobSlot {
    /// Whether this slot's work publishes a proposal the window may still change its mind
    /// about, rather than a fact that already happened outside this process.
    #[must_use]
    pub const fn is_compute(self) -> bool {
        matches!(self, Self::Refresh | Self::Sort)
    }
}

/// One slot's outstanding job: its identity and the caller's own way to stop it.
#[derive(Clone, Debug)]
pub struct Slot {
    pub job: JobId,
    pub cancel: CancelToken,
}

/// The workspace's bookkeeping for the jobs it dispatched.
#[derive(Debug, Default)]
pub struct ComputeJobs {
    slots: Vec<(JobSlot, Slot)>,
    /// Jobs this controller dispatched and let go of, kept only so a cancellation asked
    /// for after the fact still reaches the worker that owns the handle.
    taken: Vec<Slot>,
}

impl ComputeJobs {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Dispatches `work` for `slot` and records the job as the one that slot waits for.
    ///
    /// A slot holds one job: replacing it raises the superseded job's own cancellation, so
    /// a walk the window has stopped waiting for does not keep reading.
    pub fn dispatch(
        &mut self,
        host: &JobHost,
        slot: JobSlot,
        owner: JobOwner,
        bound: Option<JobSource>,
        work: JobWork,
    ) -> JobId {
        let cancel = CancelToken::new();
        let job = host.dispatch(JobRequest {
            owner,
            bound,
            cancel: cancel.clone(),
            work,
        });
        if let Some(previous) = self.release(slot)
            && let Some(record) = self.taken.iter().find(|record| record.job == previous)
        {
            record.cancel.cancel();
        }
        self.slots.push((slot, Slot { job, cancel }));
        job
    }

    /// The job `slot` is currently waiting for, if any.
    #[must_use]
    pub fn active(&self, slot: JobSlot) -> Option<JobId> {
        self.record(slot).map(|slot| slot.job)
    }

    /// Whether `slot` is waiting, which is what disables conflicting controls.
    #[must_use]
    pub fn busy(&self, slot: JobSlot) -> bool {
        self.active(slot).is_some()
    }

    /// Asks `slot`'s job to stop and frees the slot, so a result that is nevertheless
    /// published later cannot be acted on.
    ///
    /// This is a computation cancellation and is offered for the slots whose work
    /// publishes a proposal. A save or a launch has reached the filesystem or the process
    /// table by the time it has a result at all, and nothing here un-says that.
    pub fn cancel(&mut self, slot: JobSlot) -> Option<JobId> {
        if !slot.is_compute() {
            // A write or a spawn is not a proposal to withdraw. Nothing here can un-say a
            // committed file or an accepted process, so no cancellation is offered.
            return None;
        }
        let job = self.release(slot)?;
        if let Some(record) = self.taken.iter().find(|record| record.job == job) {
            record.cancel.cancel();
        }
        Some(job)
    }

    /// Whether stopping has already been asked for on the job `slot` is waiting for.
    #[must_use]
    pub fn cancel_requested(&self, slot: JobSlot) -> bool {
        self.record(slot)
            .is_some_and(|record| record.cancel.stopped())
    }

    /// Decides what to do with a completion the host handed over.
    ///
    /// A slot acts on the one job it is waiting for, and on nothing else. Session-binding
    /// equality is deliberately not consulted: it says a result was computed from state
    /// that is still current, not that this window still wants that job, and a superseded
    /// sort or refresh is exactly where the two come apart. An accepted result closes the
    /// slot, so it cannot be spent a second time.
    pub fn route(&mut self, completion: &JobCompletion, current: Option<JobSource>) -> Routed {
        let slot = match completion.kind {
            JobKind::Refresh => JobSlot::Refresh,
            JobKind::Sort => JobSlot::Sort,
            JobKind::Save => JobSlot::Save,
            JobKind::Launch => JobSlot::Launch,
        };
        if self.active(slot) != Some(completion.job) {
            return Routed::Superseded;
        }
        // Both gates have to hold. Newness alone would let a result computed from an
        // older list install; source equality alone would let a superseded job's result
        // install. A fact is the exception on the second gate: a write that landed did
        // land, and the workflow is the place that decides what it means.
        if completion.kind.reports_a_fact() {
            let _ = self.release(slot);
            return Routed::Accepted(completion.kind);
        }
        if completion.bound != current {
            let _ = self.release(slot);
            return Routed::Superseded;
        }
        let _ = self.release(slot);
        Routed::Accepted(completion.kind)
    }

    /// Whether a progress report belongs to a job some slot is still waiting for.
    ///
    /// The host's slot holds the newest report *published*, which is not the newest report
    /// that *belongs to the window*: an abandoned worker may still be walking, and its
    /// counts must not appear in a status line that says otherwise.
    #[must_use]
    pub fn shows_progress(&self, progress: &JobProgress) -> bool {
        self.slots.iter().any(|(_, slot)| slot.job == progress.job)
    }

    /// Dispatches a computation (a scan or a sort), whose proposal a later one may replace.
    pub fn dispatch_compute(
        &mut self,
        host: &JobHost,
        slot: JobSlot,
        owner: JobOwner,
        bound: Option<JobSource>,
        work: JobWork,
    ) -> JobId {
        self.dispatch(host, slot, owner, bound, work)
    }

    /// Dispatches a write or a spawn, whose result is a fact the workflow still owes an
    /// answer for.
    pub fn dispatch_fact(
        &mut self,
        host: &JobHost,
        slot: JobSlot,
        operation: crate::application::workflow::OperationId,
        bound: JobSource,
        work: JobWork,
    ) -> Result<JobId, Busy> {
        debug_assert!(!slot.is_compute(), "a computation uses `dispatch_compute`");
        if let Some(outstanding) = self.active(slot) {
            // The window still owes this write or spawn an answer, and so does the
            // workflow: displacing it would lose a fact that may already have happened.
            return Err(Busy {
                slot,
                job: outstanding,
            });
        }
        Ok(self.dispatch(host, slot, JobOwner::Workflow(operation), Some(bound), work))
    }

    fn record(&self, slot: JobSlot) -> Option<&Slot> {
        self.slots
            .iter()
            .find(|(kind, _)| *kind == slot)
            .map(|(_, record)| record)
    }

    /// Lets go of `slot`'s bookkeeping, keeping the handle so a cancellation asked for
    /// afterwards still reaches the worker that owns it.
    fn release(&mut self, slot: JobSlot) -> Option<JobId> {
        let index = self.slots.iter().position(|(kind, _)| *kind == slot)?;
        let record = self.slots.remove(index).1;
        let job = record.job;
        self.taken.push(record.clone());
        if self.taken.len() > MAX_KEPT_HANDLES {
            self.taken.remove(0);
        }
        Some(job)
    }
}

/// How many released handles a window keeps, which is how many cancellations can still
/// reach a job that left the window's books. Two operations deep is plenty and cannot grow.
const MAX_KEPT_HANDLES: usize = 2;

/// A row a pane is showing, and the private key a callback comes back with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RowKey(u64);

impl std::fmt::Display for RowKey {
    /// The text a window carries in its model for this row. Keys are never reused, so this
    /// is a one-way label: it identifies the row, not the record behind it.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "row-{}", self.0)
    }
}

/// The catalog entry a search shows: one physical copy, named by identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InactiveRow {
    pub key: RowKey,
    pub mod_id: crate::domain::types::ModId,
}

/// One occurrence of the active list: its position in the session's own list, which
/// is what a command has to act on, never a filtered index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveRow {
    pub key: RowKey,
    pub position: usize,
    /// The 1-based number the pane draws, which moves with the filter.
    pub ordinal: usize,
    pub token: String,
    pub mod_id: Option<crate::domain::types::ModId>,
}

/// Why a callback's row key was not acted on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stale {
    /// The key was never handed out by this projection.
    Unknown,
    /// The projection was built against a session state that has since moved.
    Rebound,
}

/// The rows two panes are showing, and the private table that says what each key means.
#[derive(Debug, Default)]
pub struct Projection {
    inactive: Vec<InactiveRow>,
    active: Vec<ActiveRow>,
    /// The session state every key in this table was read from.
    bound: Option<JobSource>,
}

impl Projection {
    /// Rebuilds both panes' rows from `session` and its own catalog, under the two search
    /// texts.
    ///
    /// Filtering narrows what is drawn and changes nothing else: a row keeps the working-list
    /// position it was built from, so a command issued through a filtered view reaches the same
    /// occurrence it would have reached unfiltered. The pane order is the catalog's and the
    /// list's own — the controller never imposes a sort the domain did not decide.
    #[must_use]
    pub fn build(
        session: &crate::application::session::Session,
        inactive_search: &str,
        active_search: &str,
    ) -> Self {
        let active_ids: Vec<&ModId> = session
            .working_entries()
            .iter()
            .filter_map(|entry| entry.mod_id())
            .collect();
        let inactive = session
            .catalog()
            .mods()
            .iter()
            .filter(|record| !active_ids.contains(&&record.id))
            .filter(|record| matches(inactive_search, &searchable(record)))
            .map(|record| InactiveRow {
                key: RowKey::next(),
                mod_id: record.id.clone(),
            })
            .collect();

        let active = session
            .working_entries()
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                let mut haystack = entry
                    .mod_id()
                    .and_then(|mod_id| session.catalog().get(mod_id))
                    .map(searchable)
                    .unwrap_or_default();
                haystack.push(entry.original_token().to_string());
                matches(active_search, &haystack)
            })
            .enumerate()
            .map(|(ordinal, (position, entry))| ActiveRow {
                key: RowKey::next(),
                position,
                ordinal: ordinal + 1,
                token: entry.original_token().to_string(),
                mod_id: entry.mod_id().cloned(),
            })
            .collect();

        Self {
            inactive,
            active,
            bound: Some(JobSource::of(session)),
        }
    }

    /// The available-mods pane, in the catalog's own row order.
    #[must_use]
    pub fn inactive(&self) -> &[InactiveRow] {
        &self.inactive
    }

    /// The session state this projection was drawn from, if it was drawn from one at all.
    ///
    /// The window uses it to decide whether a repaint has to re-issue row keys: a frame that
    /// changes nothing about the list must not invalidate the rows it just handed out.
    #[must_use]
    pub fn source(&self) -> Option<JobSource> {
        self.bound
    }

    /// The active-list pane, in load order.
    #[must_use]
    pub fn active(&self) -> &[ActiveRow] {
        &self.active
    }

    /// Resolves an available-row key to the physical copy it was showing.
    ///
    /// The answer is a [`ModId`] plus the projection's own check that the session is still
    /// the one the row was drawn from: a copy identity is only meaningful against the catalog
    /// generation it came out of, and that generation changes with the session it belongs to.
    pub fn resolve_inactive(
        &self,
        session: &crate::application::session::Session,
        key: RowKey,
    ) -> Result<ModId, Stale> {
        let row = self
            .inactive
            .iter()
            .find(|row| row.key == key)
            .ok_or(Stale::Unknown)?;
        self.reject_moved(session)?;
        Ok(row.mod_id.clone())
    }

    /// Resolves an active-row key to the working-list position it occupies.
    ///
    /// Two occurrences of one copy are distinguishable only by position, which is why the
    /// position is what comes back — never the filtered row number, and never a bare identity.
    pub fn resolve_active(
        &self,
        session: &crate::application::session::Session,
        key: RowKey,
    ) -> Result<usize, Stale> {
        let row = self
            .active
            .iter()
            .find(|row| row.key == key)
            .ok_or(Stale::Unknown)?;
        self.reject_moved(session)?;
        Ok(row.position)
    }

    /// True when this table was drawn from the session state that is current right now.
    fn reject_moved(&self, session: &crate::application::session::Session) -> Result<(), Stale> {
        if self.bound == Some(JobSource::of(session)) {
            Ok(())
        } else {
            Err(Stale::Rebound)
        }
    }
}

/// Row keys are handed out from this sequence and never reused, so a callback carrying an
/// older projection's key is unknown rather than accidentally valid.
static NEXT_ROW_KEY: AtomicU64 = AtomicU64::new(1);

impl RowKey {
    fn next() -> Self {
        Self(NEXT_ROW_KEY.fetch_add(1, Ordering::Relaxed))
    }
}

/// Whether one row survives a search: case-insensitive containment over the text a user can
/// see. A search that matches nothing narrows the pane to nothing; it never re-orders or
/// re-picks a copy.
fn matches(search: &str, haystack: &[String]) -> bool {
    let needle = search.trim().to_lowercase();
    needle.is_empty() || haystack.iter().any(|text| text.contains(&needle))
}

/// The searchable text of one installed copy: its directory identity, its declared name and
/// its package id, whatever of those it has.
fn searchable(record: &ModMetadata) -> Vec<String> {
    let mut texts = vec![record.id.to_string()];
    texts.extend(record.parsed.name.clone());
    texts.extend(
        record
            .parsed
            .package_id
            .as_ref()
            .map(|package_id| package_id.as_str().to_string()),
    );
    texts
}

/// What a native close request is answered with, before any window is touched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseIntent {
    /// The window may go away: the workflow emitted `Close`.
    Allow,
    /// The window stays: a prompt, a write, or nothing at all is outstanding.
    Hold,
}

/// Maps the effects a reduce call produced onto window-level behaviour.
#[must_use]
pub fn close_intent(effects: &[WorkflowEffect]) -> CloseIntent {
    // The native close handler cancels the window until this answer says otherwise, and
    // only one effect ever can: a prompt, a write, a spawn or a failure report all leave
    // the window standing with whatever the user was looking at still on screen.
    if effects
        .iter()
        .any(|effect| matches!(effect, WorkflowEffect::Close))
    {
        CloseIntent::Allow
    } else {
        CloseIntent::Hold
    }
}

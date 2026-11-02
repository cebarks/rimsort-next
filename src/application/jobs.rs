//! Background jobs: one owned unit of work per worker thread, and results that still
//! say who asked for them.
//!
//! A job carries its input as owned data and hands back owned data. Nothing from the
//! UI layer crosses into a worker, and a worker never touches the editing session:
//! the surface here is the whole boundary. This module holds no UI type at all, which
//! is what lets the boundary be tested as plain library code.
//!
//! Cancellation is cooperative. A token is read before a job starts and, for a scan
//! or a sort, at every boundary the scanner and the sorter reach. A check cannot un-do
//! what the operating system already did, so a save and a launch check once, before
//! their one external step, and after that step report only what happened.
//!
use std::cell::Cell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use crate::application::session::Session;
use crate::application::workflow::{
    LaunchOutcome, OperationFailure, OperationId, RefreshOutcome, RefreshReplacement,
    RefreshTarget, SaveOutcome,
};
use crate::domain::catalog::{ActiveEntry, Catalog};
use crate::domain::rules::Ruleset;
#[cfg(test)]
use crate::domain::sorting::sort_active_checked;
use crate::domain::sorting::{SortError, SortStop, sort_active_with_rules_checked};
use crate::domain::types::{Error, GameVersion};
use crate::domain::validation::{Issue, validate_with_rules};
use crate::integrations::config::{ConfigSnapshot, PreparedSave, commit_save, parse_config};
use crate::integrations::launch::{LaunchCommand, spawn_launch};
use crate::integrations::scan::{ScanStop, scan_install_checked};

/// Jobs take their identity from this sequence, so no two jobs share one.
static NEXT_JOB: AtomicU64 = AtomicU64::new(1);

/// One dispatched job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct JobId(u64);

impl JobId {
    fn take() -> Self {
        Self(NEXT_JOB.fetch_add(1, Ordering::Relaxed))
    }
}

/// The session state a job's input was read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobSource {
    /// The loaded session the input came from.
    pub generation: u64,
    /// The revision the input was read at.
    pub revision: u64,
}

impl JobSource {
    /// Reads the identity of `session` as it stands right now.
    #[must_use]
    pub fn of(session: &Session) -> Self {
        Self {
            generation: session.generation(),
            revision: session.revision(),
        }
    }
}

/// Who asked for a job, which is what its result answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobOwner {
    /// One effect a workflow emitted. The identity is the effect's own, copied here
    /// and copied back on the result; a job never invents or merges one.
    Workflow(OperationId),
    /// The first read of an installation, before any workflow operation exists.
    Startup,
}

/// The caller's way to stop a job that has not published anything yet.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks every job holding this token to stop at its next boundary.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    #[must_use]
    pub fn stopped(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Which kind of work a job does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JobKind {
    Refresh,
    Sort,
    Save,
    Launch,
}

impl JobKind {
    /// The word a status line uses for this kind of job.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Refresh => "refresh",
            Self::Sort => "sort",
            Self::Save => "save",
            Self::Launch => "launch",
        }
    }

    /// Whether this kind of job can report something that already happened outside
    /// this process, which is what makes its result undroppable at delivery.
    pub(crate) const fn reports_a_fact(self) -> bool {
        matches!(self, Self::Save | Self::Launch)
    }
}

/// The stage a worker is in, which is what a progress report names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobStage {
    /// Walking the candidate's mod folders.
    ReadingInstall,
    /// Reading the candidate's `ModsConfig.xml`.
    ReadingConfig,
    /// Indexing the copies the walk found.
    BuildingCatalog,
    /// Reporting what the working list declares.
    Validating,
    /// Putting the working list into load order.
    Ordering,
    /// Writing the prepared document.
    Writing,
    /// Handing the captured command to the operating system.
    Spawning,
}

/// One job's newest progress report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobProgress {
    pub job: JobId,
    pub owner: JobOwner,
    pub kind: JobKind,
    pub stage: JobStage,
    /// How many cooperative steps this job has taken *in this stage*. A scan's folder
    /// reads and a sort's levels are not one unit, so the count restarts per stage.
    pub steps: u64,
}

/// The list a validation/sort job was handed, taken once from the dispatching session.
///
/// The catalog is shared by reference, never rebuilt and never copied once per row.
/// The list and the release are owned copies, so the worker reads a snapshot instead
/// of reaching into a session the user may already be editing.
#[derive(Clone, Debug)]
pub struct ModListSnapshot {
    pub catalog: Arc<Catalog>,
    pub rules: Arc<Ruleset>,
    pub active: Vec<ActiveEntry>,
    pub target_version: GameVersion,
}

impl ModListSnapshot {
    /// Copies what a validation/sort job needs, in one pass over the session.
    #[must_use]
    pub fn from_session(session: &Session) -> Self {
        Self {
            catalog: session.catalog_arc(),
            rules: session.rules_arc(),
            active: session.working_entries().to_vec(),
            target_version: session.target_version().clone(),
        }
    }
}

/// The document a save job was handed: the destination plus the two halves a
/// `WorkflowEffect::Save` carried, taken verbatim. Nothing here re-prepares anything.
#[derive(Clone, Debug)]
pub struct SaveRequest {
    pub path: PathBuf,
    pub baseline: ConfigSnapshot,
    pub prepared: Box<PreparedSave>,
}

/// One unit of work for a worker thread.
#[derive(Debug)]
pub struct JobRequest {
    pub owner: JobOwner,
    /// The session state the input was read from, when a session state was read at
    /// all. The first read of an installation happens before any session exists, so it
    /// is bound to nothing and can never be stale against one.
    pub bound: Option<JobSource>,
    pub cancel: CancelToken,
    pub work: JobWork,
}

/// The four kinds of work a worker can be handed.
#[derive(Debug)]
pub enum JobWork {
    /// Re-read this captured candidate completely.
    Refresh(RefreshTarget),
    /// Validate and sort this snapshot.
    Sort(ModListSnapshot),
    /// Write this prepared document against its own baseline.
    Save(SaveRequest),
    /// Start exactly this command.
    Launch(LaunchCommand),
}

impl JobWork {
    fn kind(&self) -> JobKind {
        match self {
            Self::Refresh(_) => JobKind::Refresh,
            Self::Sort(_) => JobKind::Sort,
            Self::Save(_) => JobKind::Save,
            Self::Launch(_) => JobKind::Launch,
        }
    }
}

/// The two things a validation/sort pass answers about the list it was given.
#[derive(Clone, Debug)]
pub struct SortReport {
    pub issues: Vec<Issue>,
    pub order: Result<Vec<ActiveEntry>, SortError>,
}

/// What a job's work came to.
#[derive(Debug)]
pub enum JobOutcome {
    /// The refresh read the candidate, or could not. Nothing is installed here:
    /// installing is the caller's decision, reached through the workflow's own
    /// `InstallRefresh` path.
    Refreshed(RefreshOutcome),
    /// Validation finished, and sorting produced an order or refused to.
    Validated(SortReport),
    /// The write was performed, or was not. `Committed` is a filesystem fact.
    Saved(SaveOutcome),
    /// The process was accepted, or was refused. `Spawned` is a fact.
    Launched(LaunchOutcome),
    /// A cancellation check stopped this job before it published any work product.
    /// Nothing happened outside this process, so there is no fact to report.
    Cancelled,
    /// The job never ran at all: no worker thread could be started for it. Nobody
    /// asked it to stop, which is why this is not `Cancelled`.
    Failed(OperationFailure),
}

/// A finished job, bound to what asked for it.
#[derive(Debug)]
pub struct JobCompletion {
    pub job: JobId,
    pub owner: JobOwner,
    pub kind: JobKind,
    /// The binding the request carried, copied back.
    pub bound: Option<JobSource>,
    pub outcome: JobOutcome,
}

/// What the delivery boundary decided about a finished job.
#[derive(Debug)]
pub enum Delivered {
    /// This result may be acted on: it is bound to the session state the caller says
    /// is current, or it reports a fact that happened whatever the list does now.
    Current(JobCompletion),
    /// A refresh or sort result bound to a session state that is gone. Its payload is
    /// dropped here, so nothing can be installed or reordered from it.
    Discarded {
        job: JobId,
        owner: JobOwner,
        kind: JobKind,
        bound: Option<JobSource>,
    },
}

/// What handing a sort job's order to a session came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortApplied {
    /// The order is the working list now, and every row moved with its own provenance.
    Published,
    /// The working list already had this order, so nothing moved and no revision was
    /// spent.
    Unchanged,
    /// This order may not be published into this session: the result is bound to another
    /// session state, or its order is not a rearrangement of the rows this session holds,
    /// or it is not a sort result at all. The list, each row's provenance, the revision
    /// and the dirty state are exactly as they were.
    Refused,
}

/// Publishes a sort job's load order into the session its input was read from.
///
/// This is the whole sort handoff, and both guards have to hold: the completion must be
/// bound to this session's current generation and revision, and the order it carries must
/// rearrange exactly the rows the session holds now. Either refusal leaves the session
/// untouched, which is what makes it impossible for an order computed before an edit to
/// overwrite what the user did after it. A completion whose outcome is not a sort result
/// is refused rather than reinterpreted: nothing here republishes work under an identity
/// it did not arrive with, and an order the sorter refused stays refused.
#[must_use]
pub fn apply_sort_order(session: &mut Session, completion: &JobCompletion) -> SortApplied {
    if completion.bound != Some(JobSource::of(session)) {
        return SortApplied::Refused;
    }
    let JobOutcome::Validated(report) = &completion.outcome else {
        return SortApplied::Refused;
    };
    let Ok(order) = &report.order else {
        return SortApplied::Refused;
    };
    match session.apply_sorted(order) {
        Ok(true) => SortApplied::Published,
        Ok(false) => SortApplied::Unchanged,
        Err(_) => SortApplied::Refused,
    }
}

/// What a worker shares with its host.
#[derive(Debug)]
struct HostInner {
    /// Whether the caller has stopped listening, which is what makes a late result
    /// something to drop rather than something to queue.
    sealed: AtomicBool,
    /// How many finished results were dropped because nobody was listening.
    discarded: AtomicU64,
    /// Finished jobs, in arrival order, one per dispatched job.
    results: Mutex<VecDeque<JobCompletion>>,
    /// The newest progress report any worker published.
    progress: Mutex<Option<JobProgress>>,
}

impl HostInner {
    /// Hands a finished job to the caller, or accounts for it as dropped if the caller
    /// has gone.
    ///
    /// The sealed decision is taken *with the queue lock held*, so a worker can neither
    /// enqueue behind a shutdown that already returned nor be counted twice. Dropping is
    /// the whole of the shutdown story: a worker never panics because nobody is reading,
    /// and a job whose work already happened outside this process happened regardless of
    /// who hears about it.
    fn post(&self, completion: JobCompletion) {
        let mut results = Self::lock(&self.results);
        if self.sealed() {
            self.discarded.fetch_add(1, Ordering::Relaxed);
            return;
        }
        results.push_back(completion);
    }

    /// Keeps the newest progress report, dropping the one it replaces.
    ///
    /// Same rule as [`HostInner::post`]: whether the caller is still listening is decided
    /// while holding the cell the report is written to.
    fn report(&self, progress: JobProgress) {
        let mut slot = Self::lock(&self.progress);
        if self.sealed() {
            return;
        }
        *slot = Some(progress);
    }

    /// Stops handing anything on and releases what is already queued.
    ///
    /// The payload of a finished refresh or sort is a whole catalog and configuration, and
    /// the only other reference to this state can be a worker still blocked inside one
    /// filesystem read. Clearing here is what keeps a window that has gone away from
    /// pinning an installation it will never show. The flag is set under the same lock a
    /// poster takes, so nothing slips in between the decision and the clearing.
    fn seal_and_clear(&self) {
        // The flag is set while the queue lock is held, so a poster that arrives after this
        // point sees it set and accounts for its result instead of enqueueing behind the
        // clear. Releasing the lock first would reopen exactly the window being closed here.
        let mut results = Self::lock(&self.results);
        self.sealed.store(true, Ordering::Relaxed);
        results.clear();
        *Self::lock(&self.progress) = None;
    }

    /// Takes the oldest completion that has arrived, if one has.
    fn take_result(&self) -> Option<JobCompletion> {
        Self::lock(&self.results).pop_front()
    }

    /// Takes the newest progress report, leaving no other one behind.
    fn take_progress(&self) -> Option<JobProgress> {
        Self::lock(&self.progress).take()
    }

    fn sealed(&self) -> bool {
        self.sealed.load(Ordering::Relaxed)
    }

    /// Locks a host cell.
    ///
    /// A poisoned lock still holds the same data, and a caller that is closing the
    /// application must not panic because some other job panicked while holding it, so
    /// the poison is taken as the value rather than as a failure.
    fn lock<'cell, T>(cell: &'cell Mutex<T>) -> MutexGuard<'cell, T> {
        cell.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A coalesced "a worker said something" notice, installed by the thread that owns the
/// window.
///
/// The contract is exactly one pending notice at a time: a worker that posts while a notice
/// is already outstanding adds nothing, and the receiver takes the notice before it drains
/// the queues, so a post that lands during the drain cannot be missed.
pub(crate) type Wakeup = Arc<dyn Fn() + Send + Sync + 'static>;

/// One pending notice plus the callback that delivers it.
#[derive(Default)]
struct WakeSlot {
    pending: AtomicBool,
    /// Installed once by the thread that owns the window; a second install is refused
    /// rather than silently swapping the callback a live drain is using.
    installed: std::sync::OnceLock<Wakeup>,
}

// A boxed callback has no `Debug` of its own, and taking the lock to print a host is not
// worth a deadlock, so the slot reports only what it can say without either.
impl std::fmt::Debug for WakeSlot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WakeSlot")
            .field("pending", &self.pending.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl WakeSlot {
    /// Mark the notice pending and knock at most once per pending notice.
    ///
    /// Called with no host queue lock held, so the callback it invokes is free to reach
    /// back into the host.
    fn notify(&self) {
        if self.pending.swap(true, Ordering::Relaxed) {
            return;
        }
        // Cloning out the shared callback and dropping every slot reference before the call
        // is what lets the callback reach back into the host (to drain, or to schedule work
        // on the UI thread) without deadlock and without holding a lock across an unknown
        // amount of user code.
        let Some(wakeup) = self.installed.get().map(Arc::clone) else {
            return;
        };
        wakeup();
    }

    fn take(&self) -> bool {
        self.pending.swap(false, Ordering::Relaxed)
    }
}

/// The queue of work a UI thread has handed to worker threads.
///
/// One host serves one application window. It owns the threads it started and the results
/// they posted, and no editing state at all: a background job that could reach a session
/// is the bug this shape exists to prevent.
#[derive(Debug)]
pub struct JobHost {
    inner: Arc<HostInner>,
    /// The threads this host started and has not been paid for yet.
    workers: Mutex<Vec<(JobId, JoinHandle<()>)>>,
    /// Jobs whose worker finished by panicking, kept until [`JobHost::reap_workers`] hands
    /// the receipt out, so pruning a finished thread can never lose the fact that it died.
    panicked: Mutex<Vec<JobId>>,
    wake: Arc<WakeSlot>,
}

impl Default for JobHost {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for JobHost {
    /// A host that goes away without an explicit shutdown is a caller that stopped
    /// listening: seal it and release its payloads, and wait for nothing.
    ///
    /// No join happens here. A worker still inside a filesystem read may outlive the
    /// window, and the only thing this teardown owes it is that its result is dropped and
    /// counted rather than retained by a state nobody will read.
    fn drop(&mut self) {
        self.inner.seal_and_clear();
    }
}

impl JobHost {
    /// A host with no jobs running.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(HostInner {
                sealed: AtomicBool::new(false),
                discarded: AtomicU64::new(0),
                results: Mutex::new(VecDeque::new()),
                progress: Mutex::new(None),
            }),
            workers: Mutex::new(Vec::new()),
            panicked: Mutex::new(Vec::new()),
            wake: Arc::new(WakeSlot::default()),
        }
    }

    /// Installs the callback a worker invokes when it posted something worth looking at.
    ///
    /// The callback runs on the posting thread with no host lock held, so it must do nothing
    /// but schedule work on the UI thread — for a Slint window that is
    /// `invoke_from_event_loop`. Crate-private by owner decision: the window reaches it
    /// through `ui::bridge::install_wakeup`, so this host's public surface stays as Phase 6A
    /// delivered it.
    pub(crate) fn install_wakeup(&self, wakeup: Wakeup) -> bool {
        self.wake.installed.set(wakeup).is_ok()
    }

    /// Takes the pending notice, leaving none behind.
    ///
    /// A caller drains with this first and the queues second: that order is what makes a
    /// notice coalesced without ever losing one, because a post that lands during the drain
    /// re-arms the pending flag and knocks again.
    #[must_use]
    pub(crate) fn take_wakeup(&self) -> bool {
        self.wake.take()
    }

    /// Whether [`JobHost::shutdown`] has been called.
    #[must_use]
    pub fn sealed(&self) -> bool {
        self.inner.sealed()
    }

    /// Accepts `job` and hands it to its own worker thread.
    ///
    /// The id this returns is the one that job's completion and progress reports carry.
    /// Starting the thread is all this does: nothing is read, written or started here,
    /// and the caller keeps its session to itself.
    pub fn dispatch(&self, job: JobRequest) -> JobId {
        let id = JobId::take();
        let kind = job.work.kind();
        let owner = job.owner;
        let bound = job.bound;
        let inner = Arc::clone(&self.inner);
        let running = Arc::clone(&self.inner);
        let wake = Arc::clone(&self.wake);
        let started = std::thread::Builder::new()
            .name(format!("rimsort-{}-job-{}", kind.word(), id.0))
            .spawn(move || {
                let completion = run_job(&running, id, job, &wake);
                running.post(completion);
                wake.notify();
            });
        match started {
            Ok(handle) => {
                let mut workers = HostInner::lock(&self.workers);
                let mut panicked = HostInner::lock(&self.panicked);
                // A thread that has already returned is paid for here: its panic receipt is
                // kept and its slot freed, so pruning never silently drops the fact that a
                // worker died. Threads still running are left alone, because this is the UI
                // thread and nothing here may wait for a job.
                reap_finished(&mut workers, &mut panicked);
                workers.push((id, handle));
            }
            // No thread means no work was done, which is a failure the caller has to hear
            // about rather than a job that merely appears to have been accepted. The
            // identities this completion echoes are the ones the request carried, so the
            // caller can still tell which operation to report it for.
            Err(source) => {
                inner.post(JobCompletion {
                    job: id,
                    owner,
                    kind,
                    bound,
                    outcome: JobOutcome::Failed(OperationFailure::new(format!(
                        "no worker thread could be started for this {} job: {source}",
                        kind.word()
                    ))),
                });
                self.wake.notify();
            }
        }
        id
    }

    /// Takes the next finished job and decides what may be done with it.
    ///
    /// `current` is the session state the caller is delivering into, read from the live
    /// session at this moment, or `None` while no session state exists yet — which is how
    /// the first read of an installation is delivered without inventing a session to bind
    /// it to. A refresh or sort result bound to anything else is reported discarded, and
    /// its payload is dropped. A save or launch result is handed on whatever else changed,
    /// because its outcome is a fact rather than a proposal, and which operation owns that
    /// fact is the workflow's decision.
    pub fn poll_completion(&self, current: Option<JobSource>) -> Option<Delivered> {
        if self.sealed() {
            return None;
        }
        let completion = self.inner.take_result()?;
        if completion.kind.reports_a_fact() || completion.bound == current {
            return Some(Delivered::Current(completion));
        }
        let JobCompletion {
            job,
            owner,
            kind,
            bound,
            outcome: _,
        } = completion;
        Some(Delivered::Discarded {
            job,
            owner,
            kind,
            bound,
        })
    }

    /// Takes the newest progress report, if one is waiting.
    pub fn poll_progress(&self) -> Option<JobProgress> {
        if self.sealed() {
            return None;
        }
        self.inner.take_progress()
    }

    /// Stops handing results and progress to this caller and releases what is already
    /// queued, raising no cancellation and waiting for no thread.
    ///
    /// A job that has already written a file or started a process is not un-done by a
    /// window closing, and a worker that posts after this point has its result dropped
    /// where it lands and counted by [`JobHost::discarded_results`]. The queued payloads
    /// are dropped too: a worker still blocked inside one filesystem read holds the only
    /// other reference to this state, and it must not be what keeps a finished
    /// installation alive after the caller is gone.
    pub fn shutdown(&self) {
        self.inner.seal_and_clear();
    }

    /// How many finished results were dropped because the caller had stopped listening.
    ///
    /// A count for a clean exit, not a queue: the payloads are gone, and this only says
    /// that a worker was still working when the window decided to leave.
    #[must_use]
    pub fn discarded_results(&self) -> u64 {
        self.inner.discarded.load(Ordering::Relaxed)
    }

    /// Waits for every thread this host started and reports the jobs whose worker panicked
    /// instead of returning, each exactly once.
    ///
    /// An empty result means every worker this host still knew about finished by returning.
    /// This is a deliberate wait, not a shutdown step: [`JobHost::shutdown`] and
    /// [`JobHost::dispatch`] never join a running worker, because a worker inside one
    /// filesystem read can outlive the window and the UI thread must not stop for it. Panic
    /// receipts from threads that had already finished are included here even if a later
    /// dispatch pruned their slots.
    pub fn reap_workers(&self) -> Vec<JobId> {
        let started: Vec<(JobId, JoinHandle<()>)> =
            HostInner::lock(&self.workers).drain(..).collect();
        let mut found: Vec<JobId> = HostInner::lock(&self.panicked).drain(..).collect();
        for (id, handle) in started {
            if handle.join().is_err() && !found.contains(&id) {
                found.push(id);
            }
        }
        found
    }
}

/// Pays for the threads that have already finished, without waiting for any that have not.
///
/// A finished handle is joined right here, which cannot block, so the panic receipt of a
/// worker that died is kept instead of being thrown away with the slot it ran on.
fn reap_finished(workers: &mut Vec<(JobId, JoinHandle<()>)>, panicked: &mut Vec<JobId>) {
    let mut left = 0;
    while left < workers.len() {
        if workers[left].1.is_finished() {
            let (id, handle) = workers.remove(left);
            if handle.join().is_err() && !panicked.contains(&id) {
                panicked.push(id);
            }
        } else {
            left += 1;
        }
    }
}

/// Carries out one job and returns its completion.
fn run_job(
    inner: &Arc<HostInner>,
    job: JobId,
    request: JobRequest,
    wake: &Arc<WakeSlot>,
) -> JobCompletion {
    let kind = request.work.kind();
    let reporter = Reporter::new(inner, wake, job, request.owner, kind);
    // The caller's token, read through one predicate. A work function asks it at its own
    // boundaries, and the last of those is the one just before a computed product is
    // published: a scan or an order that was stopped while its final stage ran publishes
    // nothing. A save and a launch keep their single check before the side effect and
    // nothing after it, because their outcome is a fact rather than a product.
    let stopped = || request.cancel.stopped();
    let outcome = match &request.work {
        JobWork::Refresh(target) => run_refresh(target, &stopped, &reporter),
        JobWork::Sort(snapshot) => run_sort(snapshot, &stopped, &reporter),
        JobWork::Save(save) => run_save(save, &stopped, &reporter),
        JobWork::Launch(command) => run_launch(command, &stopped, &reporter),
    };
    JobCompletion {
        job,
        owner: request.owner,
        kind,
        bound: request.bound,
        outcome,
    }
}

/// One worker's publisher: it knows the stage counts and nothing else.
///
/// The reporter is used only from the thread that owns the job, so its counters are cells
/// rather than atomics, and the one shared thing it touches is the host's progress slot.
struct Reporter {
    inner: Arc<HostInner>,
    wake: Arc<WakeSlot>,
    job: JobId,
    owner: JobOwner,
    kind: JobKind,
    stage: Cell<JobStage>,
    steps: Cell<u64>,
}

impl Reporter {
    /// A reporter positioned at the first stage this kind of job enters.
    fn new(
        inner: &Arc<HostInner>,
        wake: &Arc<WakeSlot>,
        job: JobId,
        owner: JobOwner,
        kind: JobKind,
    ) -> Self {
        let stage = match kind {
            JobKind::Refresh => JobStage::ReadingInstall,
            JobKind::Sort => JobStage::Validating,
            JobKind::Save => JobStage::Writing,
            JobKind::Launch => JobStage::Spawning,
        };
        Self {
            inner: Arc::clone(inner),
            wake: Arc::clone(wake),
            job,
            owner,
            kind,
            stage: Cell::new(stage),
            steps: Cell::new(0),
        }
    }

    /// Enters a stage, restarting the step count, and publishes the new position.
    fn enter(&self, stage: JobStage) {
        self.stage.set(stage);
        self.steps.set(0);
        self.publish();
    }

    /// Records one cooperative step in the current stage.
    fn step(&self) {
        self.steps.set(self.steps.get() + 1);
        self.publish();
    }

    fn publish(&self) {
        self.inner.report(JobProgress {
            job: self.job,
            owner: self.owner,
            kind: self.kind,
            stage: self.stage.get(),
            steps: self.steps.get(),
        });
        self.wake.notify();
    }

    /// The check a scanner or sorter calls at each of its boundaries: record the step,
    /// then answer whether the caller has asked this job to stop.
    fn check<'boundary>(
        &'boundary self,
        stopped: &'boundary dyn Fn() -> bool,
    ) -> impl Fn() -> bool + 'boundary {
        move || {
            self.step();
            stopped()
        }
    }
}

/// Re-reads the whole captured candidate: its configuration, the installed copies that
/// configuration was resolved against, the release it is validated against, and the
/// path-aware problems that came with them.
///
/// The candidate travels with the job, so a settings form the user is still editing
/// cannot retarget a read that was already asked for. Nothing is installed here: the
/// replacement is handed back whole, and `InstallRefresh` through the workflow is the
/// only way it can become the loaded installation.
/// `stopped` is asked at every boundary the read reaches, including the last one before the
/// computed replacement is handed back: an installation that was stopped while its catalog
/// was being indexed produces no replacement, so nothing can be installed from a job the
/// caller had already cancelled.
fn run_refresh(
    target: &RefreshTarget,
    stopped: &dyn Fn() -> bool,
    report: &Reporter,
) -> JobOutcome {
    if stopped() {
        return JobOutcome::Cancelled;
    }
    report.enter(JobStage::ReadingInstall);
    let scanned = match scan_install_checked(&target.settings, &report.check(stopped)) {
        Ok(scanned) => scanned,
        Err(ScanStop::Cancelled) => return JobOutcome::Cancelled,
        Err(ScanStop::Failed(error)) => return refresh_failed(&error),
    };
    if stopped() {
        return JobOutcome::Cancelled;
    }

    report.enter(JobStage::ReadingConfig);
    let Some(path) = target.config_path() else {
        return JobOutcome::Refreshed(RefreshOutcome::Failed(OperationFailure::new(
            "the candidate names no configuration file, so nothing was read",
        )));
    };
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(source) => {
            return JobOutcome::Refreshed(RefreshOutcome::Failed(OperationFailure::new(format!(
                "{} could not be read: {source}",
                path.display()
            ))));
        }
    };
    let config = match parse_config(&bytes) {
        Ok(config) => config,
        Err(error) => return refresh_failed(&error),
    };
    if stopped() {
        return JobOutcome::Cancelled;
    }

    report.enter(JobStage::BuildingCatalog);
    let catalog = match Catalog::new(scanned.mods) {
        Ok(catalog) => catalog,
        Err(error) => return refresh_failed(&error),
    };
    let Some(target_version) = target.target_version().cloned() else {
        return JobOutcome::Refreshed(RefreshOutcome::Failed(OperationFailure::new(
            "the candidate names no target release, so its list cannot be validated",
        )));
    };
    // The last boundary of a computed product. Indexing the catalog is the final stage of
    // the read, so a stop asked for while it ran has to be consulted before anything is
    // handed back: an answer the caller cancelled is not an answer it may act on.
    if stopped() {
        return JobOutcome::Cancelled;
    }
    JobOutcome::Refreshed(RefreshOutcome::Reloaded(Box::new(RefreshReplacement {
        origin: target.clone(),
        config,
        catalog,
        target_version,
        // Every problem the read produced travels with the replacement: a refresh that met
        // a folder it could not read says so, instead of handing back a quieter
        // installation than the one on disk.
        diagnostics: scanned.diagnostics,
    })))
}

/// A refresh step that could not be completed, in the words the workflow reports.
fn refresh_failed(error: &Error) -> JobOutcome {
    JobOutcome::Refreshed(RefreshOutcome::Failed(OperationFailure::from_error(error)))
}

/// Reports what the list declares, then puts it into load order.
///
/// Both answers belong to the one job: the findings are what the user is asked to confirm
/// and the order is what a sort action publishes, and a refusal to order the list says
/// nothing about the findings. Sorting runs over the snapshot the session handed over at
/// dispatch, never over the session.
/// `stopped` is asked before the pass starts, at every boundary the sorter reaches, and
/// once more after the order is computed. That last check is the publication boundary: a
/// sort stopped while its final level was being placed reports the stop instead of an order
/// nobody asked for any more.
fn run_sort(
    snapshot: &ModListSnapshot,
    stopped: &dyn Fn() -> bool,
    report: &Reporter,
) -> JobOutcome {
    if stopped() {
        return JobOutcome::Cancelled;
    }
    report.enter(JobStage::Validating);
    let issues = validate_with_rules(
        &snapshot.catalog,
        &snapshot.active,
        &snapshot.target_version,
        &snapshot.rules,
    );
    if stopped() {
        return JobOutcome::Cancelled;
    }
    report.enter(JobStage::Ordering);
    let order = match sort_active_with_rules_checked(
        &snapshot.catalog,
        &snapshot.active,
        &snapshot.rules,
        &report.check(stopped),
    ) {
        Ok(order) => Ok(order),
        Err(SortStop::Cancelled) => return JobOutcome::Cancelled,
        Err(SortStop::Refused(error)) => Err(error),
    };
    if stopped() {
        return JobOutcome::Cancelled;
    }
    JobOutcome::Validated(SortReport { issues, order })
}

/// Writes the prepared document, or reports that it could not be written.
fn run_save(save: &SaveRequest, stopped: &dyn Fn() -> bool, report: &Reporter) -> JobOutcome {
    // The only check this job can honestly make: past this line the bytes are on their way
    // to the game's configuration, and the answer is either that they landed or that they
    // did not. A write that landed is never reported cancelled, because nothing here
    // un-writes it, and a stop asked for afterwards is never consulted again.
    if stopped() {
        return JobOutcome::Cancelled;
    }
    report.enter(JobStage::Writing);
    JobOutcome::Saved(
        match commit_save(&save.path, &save.baseline, &save.prepared) {
            Ok(committed) => SaveOutcome::Committed(committed),
            Err(error) => SaveOutcome::Failed(OperationFailure::from_error(&error)),
        },
    )
}

/// Hands the captured command to the operating system.
///
/// The command is used exactly as the workflow built it: program, separate arguments and
/// start directory, never re-parsed, never rejoined into one string, never rebuilt from
/// settings that may have been edited since.
fn run_launch(
    command: &LaunchCommand,
    stopped: &dyn Fn() -> bool,
    report: &Reporter,
) -> JobOutcome {
    if stopped() {
        return JobOutcome::Cancelled;
    }
    report.enter(JobStage::Spawning);
    JobOutcome::Launched(match spawn_launch(command) {
        Ok(child) => {
            // Dropping the handle is the documented standard-library behavior: the process
            // keeps running, this build neither waits for it nor kills it, and
            // `process_id` is the identity the operating system accepted.
            LaunchOutcome::Spawned {
                process_id: child.id(),
            }
        }
        Err(error) => LaunchOutcome::Failed(OperationFailure::from_error(&error)),
    })
}

#[cfg(test)]
mod teardown_tests {
    //! What shutdown and dropping the host owe the caller and the workers. The payload a
    //! finished refresh or sort carries is a whole catalog, so "sealed means sealed" has to
    //! also release what is already queued: a blocked worker holding one more `Arc` on the
    //! host must not be what keeps a finished installation in memory after the window is
    //! gone. These cases run on the host's own cells, never on a substitute queue.
    use super::*;
    use crate::domain::types::{ModId, ModMetadata, PackageId, ParsedMetadata, Source};

    fn version() -> GameVersion {
        GameVersion::parse("1.5.4171").expect("the target must parse")
    }

    /// One installed copy, so a sort job has real work to do and a real product to hand back.
    pub(super) fn snapshot() -> ModListSnapshot {
        let package = PackageId::normalize("only.mod").expect("the package id must normalize");
        let catalog = Catalog::new(vec![ModMetadata::new(
            ModId::new("/mods/only"),
            Source::Local,
            ParsedMetadata {
                package_id: Some(package),
                name: Some("Only".to_string()),
                ..ParsedMetadata::default()
            },
        )])
        .expect("the catalog must build");
        ModListSnapshot {
            catalog: Arc::new(catalog),
            rules: Arc::default(),
            active: vec![ActiveEntry::Resolved {
                mod_id: ModId::new("/mods/only"),
                original_token: "only.mod".to_string(),
            }],
            target_version: version(),
        }
    }

    pub(super) fn bound() -> JobSource {
        JobSource {
            generation: 7,
            revision: 0,
        }
    }

    pub(super) fn dispatch_test(host: &JobHost, work: JobWork) -> JobId {
        host.dispatch(JobRequest {
            owner: JobOwner::Startup,
            bound: Some(bound()),
            cancel: CancelToken::new(),
            work,
        })
    }

    #[test]
    fn shutdown_releases_the_payloads_a_finished_job_left_behind() {
        let host = JobHost::new();
        dispatch_test(&host, JobWork::Sort(snapshot()));
        assert!(
            host.reap_workers().is_empty(),
            "the job must finish by returning"
        );
        // Both a result and a progress report are in the host's cells right now.
        assert!(
            host.inner.take_result().is_some(),
            "a finished job's completion is queued"
        );
        dispatch_test(&host, JobWork::Sort(snapshot()));
        assert!(
            host.reap_workers().is_empty(),
            "the second job must finish by returning too"
        );
        assert!(
            host.inner.take_progress().is_some(),
            "and the worker published progress"
        );

        // Now leave a fresh result queued and shut down without polling it.
        let queued = dispatch_test(&host, JobWork::Sort(snapshot()));
        assert!(
            host.reap_workers().is_empty(),
            "the third job must finish by returning"
        );
        host.shutdown();

        assert!(
            host.inner.take_result().is_none(),
            "shutdown must release the heavy payload {queued:?} left in the queue, because a \
             worker still blocked in a read holds the only other reference to this state"
        );
        assert!(
            host.inner.take_progress().is_none(),
            "and the progress report a late worker left behind"
        );
        assert_eq!(
            host.discarded_results(),
            0,
            "a payload that was already queued is not counted as a late arrival"
        );
    }

    #[test]
    fn dropping_the_host_seals_and_stops_retaining_results_without_waiting() {
        let host = JobHost::new();
        let inner = Arc::clone(&host.inner);
        let job = dispatch_test(&host, JobWork::Sort(snapshot()));
        assert!(
            host.reap_workers().is_empty(),
            "the job finishes before the caller goes away"
        );

        drop(host);

        assert!(
            inner.sealed(),
            "an abandoned host must stop handing results to a caller that is gone"
        );
        // A worker that is only now finishing its read posts into this same state.
        let too_late = inner.take_result();
        assert!(
            too_late.is_none(),
            "the queued completion of {job:?} was released with the host, not kept alive by it"
        );
    }
}

#[cfg(test)]
mod final_publication_tests {
    //! The last boundary of a computed product. A refresh's catalog indexing and a sort's
    //! final level are the last work before an actionable replacement or order exists, so
    //! the tests drive the real `run_refresh` / `run_sort` bodies with a counted stop that
    //! is raised at the *last* boundary the job asks - a stop that lands while the final
    //! stage is running must publish nothing. The boundary count is measured from the same
    //! functions rather than hard-coded, so nothing here depends on how fast anything runs,
    //! and no case blocks on anything a thread cannot leave.
    use super::*;
    use crate::domain::types::{ModId, ModMetadata, PackageId, ParsedMetadata, Source};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn version() -> GameVersion {
        GameVersion::parse("1.5.4171").expect("the target must parse")
    }

    fn host() -> Arc<HostInner> {
        Arc::new(HostInner {
            sealed: AtomicBool::new(false),
            discarded: AtomicU64::new(0),
            results: Mutex::new(VecDeque::new()),
            progress: Mutex::new(None),
        })
    }

    /// Two installed copies, one of which declares it loads after the other, so the sort
    /// pass has a real order to compute and a deterministic number of boundaries.
    fn snapshot() -> ModListSnapshot {
        let make = |path: &str, package: &str, after: Option<&str>| {
            let mut parsed = ParsedMetadata {
                package_id: Some(
                    PackageId::normalize(package).expect("the package id must normalize"),
                ),
                name: Some(package.to_string()),
                ..ParsedMetadata::default()
            };
            if let Some(after) = after {
                parsed.load_after =
                    vec![PackageId::normalize(after).expect("the package id must normalize")];
            }
            ModMetadata::new(ModId::new(path), Source::Local, parsed)
        };
        let catalog = Catalog::new(vec![
            make("/mods/provider", "provider.mod", None),
            make("/mods/dependent", "dependent.mod", Some("provider.mod")),
        ])
        .expect("the catalog must build");
        ModListSnapshot {
            catalog: Arc::new(catalog),
            rules: Arc::default(),
            active: vec![
                ActiveEntry::Resolved {
                    mod_id: ModId::new("/mods/dependent"),
                    original_token: "dependent.mod".to_string(),
                },
                ActiveEntry::Resolved {
                    mod_id: ModId::new("/mods/provider"),
                    original_token: "provider.mod".to_string(),
                },
            ],
            target_version: version(),
        }
    }

    /// A counted stop that answers "keep going" until the `stop_after`th time it is asked,
    /// or never, when no limit was set.
    struct Stops {
        asked: AtomicUsize,
        stop_after: Option<usize>,
    }

    impl Stops {
        fn never() -> Self {
            Self {
                asked: AtomicUsize::new(0),
                stop_after: None,
            }
        }

        fn at(stop_after: usize) -> Self {
            Self {
                asked: AtomicUsize::new(0),
                stop_after: Some(stop_after),
            }
        }

        fn predicate(&self) -> impl Fn() -> bool + '_ {
            || {
                let count = self.asked.fetch_add(1, Ordering::Relaxed) + 1;
                self.stop_after.is_some_and(|limit| count >= limit)
            }
        }

        fn asked(&self) -> usize {
            self.asked.load(Ordering::Relaxed)
        }
    }

    fn sort_with(stops: &Stops) -> JobOutcome {
        run_sort(&snapshot(), &stops.predicate(), &reporter(JobKind::Sort))
    }

    #[test]
    fn an_untouched_sort_publishes_the_order_it_computed() {
        let stops = Stops::never();
        let outcome = sort_with(&stops);
        assert!(
            matches!(outcome, JobOutcome::Validated(_)),
            "a pass nobody interrupted must report its order: {outcome:?}"
        );
        assert!(
            stops.asked() >= 4,
            "the pass asked its boundaries: {} times",
            stops.asked()
        );
    }

    /// The boundaries `run_sort` asks, counted from its own parts rather than from the run
    /// under test: one before validating, one after validating, the sorter's own boundaries,
    /// and the last one after an order exists. That final ask is the publication boundary, so
    /// a pass missing it would total one fewer and the stop below would never be reached.
    fn sort_publication_boundary(snapshot: &ModListSnapshot) -> usize {
        let sorter = AtomicUsize::new(0);
        let outcome = sort_active_checked(&snapshot.catalog, &snapshot.active, &|| {
            sorter.fetch_add(1, Ordering::Relaxed);
            false
        });
        assert!(
            outcome.is_ok(),
            "the sorter must order this fixture for the arithmetic to mean anything: {outcome:?}"
        );
        2 + sorter.load(Ordering::Relaxed) + 1
    }

    #[test]
    fn a_stop_raised_after_the_order_exists_publishes_no_order() {
        let snapshot = snapshot();
        let stop_at = sort_publication_boundary(&snapshot);

        let late = Stops::at(stop_at);
        let outcome = run_sort(&snapshot, &late.predicate(), &reporter(JobKind::Sort));
        assert!(
            matches!(outcome, JobOutcome::Cancelled),
            "a sort stopped at the boundary after its order was computed must publish nothing, \
             got {outcome:?}"
        );
        assert_eq!(
            late.asked(),
            stop_at,
            "and it stopped at that boundary rather than running further"
        );
    }

    #[test]
    fn a_stop_raised_during_the_catalog_index_publishes_no_replacement() {
        let install = TestInstall::new();
        let settings = install.settings();
        // The same arithmetic for the read: one check before the walk, the walk's own
        // boundaries, one after it, one after the config, and the last one after the catalog
        // is indexed - the boundary a stopped final stage must be consulted at.
        let scanner = AtomicUsize::new(0);
        let scanned = scan_install_checked(&settings, &|| {
            scanner.fetch_add(1, Ordering::Relaxed);
            false
        });
        assert!(scanned.is_ok(), "this install must read cleanly");
        let stop_at = 3 + scanner.load(Ordering::Relaxed) + 1;

        let late = Stops::at(stop_at);
        let outcome = run_refresh(
            &RefreshTarget {
                settings: settings.clone(),
            },
            &late.predicate(),
            &reporter(JobKind::Refresh),
        );
        assert!(
            matches!(outcome, JobOutcome::Cancelled),
            "a read stopped while its catalog was indexed must publish nothing, got {outcome:?}"
        );
        assert_eq!(
            late.asked(),
            stop_at,
            "and it stopped at the publication boundary, not before it"
        );
    }

    /// A reporter on a throwaway host: the boundary tests drive the work functions directly,
    /// so progress publishing has somewhere to go and nothing else. The reporter keeps its
    /// own reference to that host, which is why the local can go right after.
    fn reporter(kind: JobKind) -> Reporter {
        let inner = host();
        let wake = Arc::new(WakeSlot::default());
        Reporter::new(&inner, &wake, JobId::take(), JobOwner::Startup, kind)
    }

    #[test]
    fn an_untouched_read_publishes_the_replacement_it_computed() {
        let install = TestInstall::new();
        let stops = Stops::never();
        let outcome = run_refresh(
            &RefreshTarget {
                settings: install.settings(),
            },
            &stops.predicate(),
            &reporter(JobKind::Refresh),
        );
        assert!(
            matches!(outcome, JobOutcome::Refreshed(_)),
            "a read nobody interrupted must hand back what it read: {outcome:?}"
        );
    }

    /// The smallest install the scanner accepts: a game root, its content `Data` folder
    /// with one copy, a local root with one copy and a config file of its own.
    struct TestInstall {
        temp: tempfile::TempDir,
    }

    impl TestInstall {
        fn new() -> Self {
            let temp = tempfile::TempDir::new().expect("a temporary install");
            let data = temp.path().join("game/Mods/Data");
            let local = temp.path().join("Local Mods");
            for root in [&data, &local] {
                std::fs::create_dir_all(root).expect("the layout must be creatable");
            }
            for (root, name, package) in [
                (data.as_path(), "core", "core.mod"),
                (local.as_path(), "extra", "extra.mod"),
            ] {
                let about = root.join(name).join("About");
                std::fs::create_dir_all(&about).expect("the mod folder must be creatable");
                std::fs::write(
                    about.join("About.xml"),
                    format!(
                        "<ModMetaData><packageId>{package}</packageId><name>{name}</name>\
                         <supportedVersions><li>1.5</li></supportedVersions></ModMetaData>"
                    ),
                )
                .expect("the metadata must be writable");
            }
            std::fs::write(
                temp.path().join("ModsConfig.xml"),
                "<ModsConfigData><activeMods><li>core.mod</li></activeMods></ModsConfigData>",
            )
            .expect("the config must be writable");
            Self { temp }
        }

        fn settings(&self) -> crate::integrations::settings::Settings {
            crate::integrations::settings::Settings {
                schema_version: 1,
                game_root: Some(self.temp.path().join("game")),
                content_root: Some(self.temp.path().join("game/Mods")),
                config_path: Some(self.temp.path().join("ModsConfig.xml")),
                local_mods_root: Some(self.temp.path().join("Local Mods")),
                workshop_root: None,
                target_version: Some(version()),
                executable_override: None,
                confirm_warnings: false,
            }
        }
    }
}

#[cfg(test)]
mod worker_receipt_tests {
    //! Threads are paid for as they finish, and a worker that died on the way has to stay
    //! accounted for: the documented meaning of an empty `reap_workers` result is that every
    //! worker finished by returning, and pruning a slot must not be what hides a panic.
    //! Nothing here joins a worker that is still running, because this is the UI thread's
    //! path. The panicking thread is a fixture for the bookkeeping, not a claim that a real
    //! job has ever panicked.
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// How long a fixture thread may take to reach its own end before the case calls the
    /// fixture broken. This caps what a broken fixture can cost during cleanup; it asserts
    /// nothing about how fast anything runs.
    const FIXTURE_DEADLINE: Duration = Duration::from_secs(30);

    #[test]
    fn a_finished_worker_that_panicked_keeps_its_receipt_through_a_prune() {
        let dying = JobId::take();
        let healthy = JobId::take();
        let mut workers = vec![
            (
                dying,
                std::thread::spawn(move || panic!("injected for this bookkeeping test only")),
            ),
            (healthy, std::thread::spawn(|| {})),
        ];
        // Bounded, then asserted: the spins below only ever wait on threads whose bodies
        // reach their own end immediately, and the deadline turns a broken fixture into a
        // failure instead of a parked case.
        let deadline = Instant::now() + FIXTURE_DEADLINE;
        while (!workers[0].1.is_finished() || !workers[1].1.is_finished())
            && Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        assert!(
            workers[0].1.is_finished() && workers[1].1.is_finished(),
            "both fixture threads must have reached their own end inside the cleanup watchdog"
        );
        let mut receipts = Vec::new();

        reap_finished(&mut workers, &mut receipts);

        assert_eq!(
            receipts,
            vec![dying],
            "the job that died is the one reported, and the one that returned is not"
        );
        assert!(
            workers.is_empty(),
            "and both finished slots were paid for rather than dropped"
        );

        // A second prune of an empty list cannot invent a second receipt.
        reap_finished(&mut workers, &mut receipts);
        assert_eq!(receipts, vec![dying], "a panic is reported once");
    }

    #[test]
    fn pruning_leaves_a_running_worker_alone_instead_of_waiting_for_it() {
        let running = JobId::take();
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let handle = std::thread::spawn(move || {
            let _ = started_tx.send(());
            // Bounded release: the thread frees itself even if the case forgets to, so no
            // path through this test can park a thread indefinitely.
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
        });
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the worker must have started before it is pruned");
        let mut workers = vec![(running, handle)];
        let mut receipts = Vec::new();

        reap_finished(&mut workers, &mut receipts);

        assert_eq!(
            workers.len(),
            1,
            "a thread that has not finished keeps its slot: pruning must not wait for it"
        );
        assert!(receipts.is_empty(), "nothing died, so nothing is reported");
        drop(release_tx);
        let (_id, handle) = workers
            .pop()
            .expect("the running worker still held its slot");
        assert!(
            handle.join().is_ok(),
            "the fixture thread finishes cleanly once released"
        );
    }

    #[test]
    fn a_panicked_worker_survives_the_next_dispatch_as_a_receipt_reported_once() {
        // The whole production path, not the helper on its own: a dead thread sits in the
        // host's bookkeeping, a real job is dispatched through the real `dispatch`, that
        // dispatch has to pay for the finished thread and keep the receipt, and only then
        // does a caller ask for it. Asserting the retained receipt *before* the final reap
        // is what makes this a regression test for the pruning step: dropping the prune
        // would leave the slot in the list and no receipt recorded, while dropping the
        // receipt drain in `reap_workers` would hand back an empty list. The panicking
        // thread is a fixture for this bookkeeping, not a claim that a production job dies.
        let host = JobHost::new();
        let dead = JobId::take();
        let fixture = std::thread::spawn(|| panic!("injected for this bookkeeping test only"));
        // Spinning is bounded by the cleanup watchdog below, and the finished state is then
        // asserted rather than assumed: pruning is defined to touch handles already finished,
        // so the receipt path can only be exercised once this thread has actually unwound.
        let deadline = Instant::now() + FIXTURE_DEADLINE;
        while !fixture.is_finished() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(
            fixture.is_finished(),
            "the fixture thread must have unwound inside the cleanup watchdog before it can be \
             filed as a finished worker; the deadline bounds cleanup, not speed"
        );
        HostInner::lock(&host.workers).push((dead, fixture));

        let cancel = CancelToken::new();
        cancel.cancel();
        let source = JobSource {
            generation: 4,
            revision: 0,
        };
        let living = host.dispatch(JobRequest {
            owner: JobOwner::Startup,
            bound: Some(source),
            cancel,
            // A real job with a real worker thread and a real posted completion, and no
            // side effect to wait on: the token was raised before it started.
            work: JobWork::Launch(LaunchCommand {
                program: PathBuf::from("/this-program-does-not-exist-for-this-test"),
                args: Vec::new(),
                working_directory: None,
            }),
        });

        let workers = HostInner::lock(&host.workers);
        assert!(
            !workers.iter().any(|(id, _)| *id == dead),
            "the dispatch that accepted {living:?} had to prune the finished fixture slot \
                 {dead:?}, not leave it to be dropped unexamined"
        );
        assert!(
            workers.iter().any(|(id, _)| *id == living),
            "and it kept its own slot, because that worker may still be running"
        );
        drop(workers);
        assert_eq!(
            HostInner::lock(&host.panicked).as_slice(),
            &[dead],
            "the panic was recorded as a receipt before any caller asked for it"
        );

        // This join also waits for the real job's worker, so its completion is in the queue.
        let reported = host.reap_workers();
        assert_eq!(
            reported,
            vec![dead],
            "the dead job is reported by the host, exactly once and among nothing else"
        );
        match host
            .poll_completion(Some(source))
            .expect("the real job still reports its own result")
        {
            Delivered::Current(found) => {
                assert_eq!(
                    found.job, living,
                    "and it belongs to the job that was dispatched"
                );
                assert!(
                    matches!(found.outcome, JobOutcome::Cancelled),
                    "a job whose token was raised before it started cancels itself: {:?}",
                    found.outcome
                );
            }
            Delivered::Discarded { job, .. } => {
                panic!("the host's own job was discarded as stale: {job:?}")
            }
        }

        assert!(
            host.reap_workers().is_empty(),
            "a receipt is handed out once, not every time a caller looks"
        );
        assert!(
            host.poll_completion(Some(source)).is_none(),
            "and one dispatched job still produces exactly one completion"
        );
    }
}

#[cfg(test)]
mod seal_race_tests {
    //! A worker decides to hand its result in at the same moment the caller decides to stop
    //! listening. Both orders are legitimate, and neither may lose the result or leave a
    //! payload behind unaccounted, so this case runs the pair repeatedly and asserts the
    //! invariant instead of an interleaving. No sleep, no wall clock: the only wait is for
    //! the two threads to finish their own work.
    use super::*;

    fn completion(job: JobId) -> JobCompletion {
        JobCompletion {
            job,
            owner: JobOwner::Startup,
            kind: JobKind::Sort,
            bound: Some(JobSource {
                generation: 3,
                revision: 0,
            }),
            outcome: JobOutcome::Cancelled,
        }
    }

    #[test]
    fn every_result_is_either_queued_for_the_caller_or_counted_as_dropped() {
        for round in 0..64 {
            let inner = Arc::new(HostInner {
                sealed: AtomicBool::new(false),
                discarded: AtomicU64::new(0),
                results: Mutex::new(VecDeque::new()),
                progress: Mutex::new(None),
            });
            let poster = Arc::clone(&inner);
            let sealing = Arc::clone(&inner);
            let job = JobId::take();
            let posting = std::thread::spawn(move || poster.post(completion(job)));
            let sealed = std::thread::spawn(move || sealing.seal_and_clear());
            posting.join().expect("a post never panics");
            sealed.join().expect("a seal never panics");

            let left = Arc::try_unwrap(inner)
                .expect("both workers are finished, so this is the last reference")
                .results
                .into_inner()
                .expect("the lock is uncontended once both threads are done");
            assert!(
                left.is_empty(),
                "round {round}: a sealed host holds no payload, whatever the order was"
            );
        }
    }
}

#[cfg(test)]
mod wake_seam_tests {
    //! The window's notice: one pending wake per burst, an install that happens once, and a
    //! drain order that cannot lose a post. These run on the host's own seam rather than a
    //! stand-in queue, and they use the sort work fixture from [`super::teardown_tests`], so
    //! nothing here blocks on anything a worker cannot leave.
    use super::teardown_tests::{bound, dispatch_test, snapshot};
    use super::*;

    fn counted() -> (Arc<AtomicU64>, impl Fn() + Send + Sync + 'static) {
        let knocks = Arc::new(AtomicU64::new(0));
        let seen = Arc::clone(&knocks);
        (knocks, move || {
            seen.fetch_add(1, Ordering::Relaxed);
        })
    }

    #[test]
    fn a_burst_of_posts_knocks_once_and_a_drain_sees_every_result() {
        let host = JobHost::new();
        let (knocks, wakeup) = counted();
        assert!(
            host.install_wakeup(Arc::new(wakeup)),
            "the first install wins"
        );

        for _ in 0..3 {
            dispatch_test(&host, JobWork::Sort(snapshot()));
        }
        assert!(
            host.reap_workers().is_empty(),
            "the workers must finish by returning"
        );

        let knocks_so_far = knocks.load(Ordering::Relaxed);
        assert!(
            (1..=3).contains(&knocks_so_far),
            "three posts may coalesce into one knock but never into one knock per post: {knocks_so_far}"
        );
        assert!(host.take_wakeup(), "the pending notice is taken");
        let mut drained = 0;
        while host.poll_completion(Some(bound())).is_some() {
            drained += 1;
        }
        assert_eq!(
            drained, 3,
            "taking the notice first still leaves every posted result"
        );
        assert!(
            !host.take_wakeup(),
            "and nothing is pending once the notice was taken and the queue drained"
        );
    }

    #[test]
    fn a_second_install_is_refused_rather_than_swapping_the_live_callback() {
        let host = JobHost::new();
        let (first, wake_first) = counted();
        let second = Arc::new(AtomicU64::new(0));
        assert!(host.install_wakeup(Arc::new(wake_first)));
        assert!(
            !host.install_wakeup(Arc::new(second_wake(&second))),
            "installing twice is refused instead of replacing the callback a drain is using"
        );
        dispatch_test(&host, JobWork::Sort(snapshot()));
        assert!(
            host.reap_workers().is_empty(),
            "the worker finishes by returning"
        );
        assert_eq!(
            first.load(Ordering::Relaxed),
            1,
            "and the originally installed callback is the one that knocked"
        );
        assert_eq!(
            second.load(Ordering::Relaxed),
            0,
            "the refused one never ran"
        );
    }

    fn second_wake(counter: &Arc<AtomicU64>) -> impl Fn() + Send + Sync + 'static {
        let seen = Arc::clone(counter);
        move || {
            seen.fetch_add(1, Ordering::Relaxed);
        }
    }
}

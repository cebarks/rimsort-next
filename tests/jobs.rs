//! Behavioral tests for revision-bound background jobs (Task 6, phase 6A).
//!
//! Every case builds its own temporary install and its own `ModsConfig.xml`, so no
//! test reads a real game install or anybody's configuration. The cases fall into two
//! groups, and the split is deliberate:
//!
//! - **Ownership cases** (a stale result is dropped, a foreign save result adopts
//!   nothing, a committed write is still reported after an edit) need no thread timing,
//!   because a completion's identity and its bound session state are decided when the
//!   result is delivered, and they are asserted against the workflow that has to honour
//!   them.
//! - **Cooperative-stop cases** must never hang and never guess at a duration. They
//!   wait on their own worker by joining it (`JobHost::reap_workers`), which is bounded
//!   because no job in this suite blocks on anything a thread cannot get out of, and
//!   they assert that staleness and dropping are decided when a result is *delivered*,
//!   not by an interleaving the test has to win. Where a claim really needs a worker
//!   stopped in the middle of a loop, it is proved at the loop's own boundary, in
//!   `scan.rs` and `sorting.rs`, by counting the checks that loop makes; a jobs-level
//!   case could only ask for a stop before or after the work, and this suite does not
//!   pretend that is the same claim.
//!
//! No case asserts how long work takes. The one watchdog below exists to bound what a
//! helper fixture that never ran can cost, and its assertion is about content.

use std::fs;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::{Duration, Instant};

use rimsort_rust::application::jobs::{
    CancelToken, Delivered, JobCompletion, JobHost, JobId, JobKind, JobOutcome, JobOwner,
    JobProgress, JobRequest, JobSource, JobStage, JobWork, ModListSnapshot, SaveRequest,
    SortApplied, SortReport, apply_sort_order,
};
use rimsort_rust::application::session::Session;
use rimsort_rust::application::workflow::{
    LaunchOutcome, OperationId, OperationKind, RefreshOutcome, RefreshTarget, SaveOutcome,
    Workflow, WorkflowEffect, WorkflowEvent,
};
use rimsort_rust::domain::catalog::{ActiveEntry, Catalog};
use rimsort_rust::domain::types::{
    GameVersion, ModId, ModMetadata, PackageId, ParsedMetadata, Source,
};
use rimsort_rust::integrations::config::{ConfigSnapshot, parse_config};
use rimsort_rust::integrations::launch::{LaunchCommand, Platform};
use rimsort_rust::integrations::scan::{ScanResult, scan_install};
use rimsort_rust::integrations::settings::Settings;
use rimsort_rust::ui::bridge::{Busy, ComputeJobs, JobSlot, Routed};
use tempfile::TempDir;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

const TARGET: &str = "1.5.4171";
const CORE: &str = "/game/Data";
const CORE_TOKEN: &str = "Ludeon.RimWorld";
const HARMONY: &str = "/mods/harmony";
const HARMONY_TOKEN: &str = "brrainz.harmony";
const PROVIDER_TOKEN: &str = "provider.mod";
const DEPENDENT_TOKEN: &str = "dependent.mod";

fn version(text: &str) -> GameVersion {
    GameVersion::parse(text).unwrap_or_else(|error| panic!("{text:?} must parse: {error}"))
}

fn package(raw: &str) -> PackageId {
    PackageId::normalize(raw).unwrap_or_else(|| panic!("{raw:?} must normalize"))
}

fn id(path: &str) -> ModId {
    ModId::new(path)
}

/// A usable copy of `package_id`, supporting the release every case targets.
fn record(path: &str, source: Source, package_id: &str) -> ModMetadata {
    ModMetadata::new(
        id(path),
        source,
        ParsedMetadata {
            package_id: Some(package(package_id)),
            name: Some(package_id.to_string()),
            supported_versions: vec![version("1.5")],
            ..ParsedMetadata::default()
        },
    )
}

/// A usable copy that declares it loads after `dependency`, so an unsorted list has
/// exactly one honest order.
fn record_dependent(path: &str, package_id: &str, dependency: &str) -> ModMetadata {
    let mut parsed = ParsedMetadata {
        package_id: Some(package(package_id)),
        name: Some(package_id.to_string()),
        supported_versions: vec![version("1.5")],
        ..ParsedMetadata::default()
    };
    parsed.load_after = vec![package(dependency)];
    ModMetadata::new(id(path), Source::Local, parsed)
}

fn catalog_of(records: Vec<ModMetadata>) -> Catalog {
    Catalog::new(records).expect("the catalog must build")
}

/// The catalog the sort and save cases resolve against.
fn editing_catalog() -> Catalog {
    catalog_of(vec![
        record(CORE, Source::Official, "ludeon.rimworld"),
        record("/mods/provider", Source::Local, "provider.mod"),
        record_dependent("/mods/dependent", "dependent.mod", "provider.mod"),
    ])
}

/// A `ModsConfig.xml` holding `tokens` verbatim, in this order.
fn document(tokens: &[&str]) -> String {
    let mut text = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<ModsConfigData>\n  \
         <version>1.5</version>\n  <activeMods>\n",
    );
    for token in tokens {
        text.push_str(&format!("    <li>{token}</li>\n"));
    }
    text.push_str("  </activeMods>\n</ModsConfigData>\n");
    text
}

fn tokens_of(entries: &[ActiveEntry]) -> Vec<String> {
    entries
        .iter()
        .map(ActiveEntry::original_token)
        .map(str::to_string)
        .collect()
}

/// A config file on disk, written once so a save has a real baseline to replace.
struct Config {
    directory: TempDir,
    path: PathBuf,
}

impl Config {
    fn new(tokens: &[&str]) -> Self {
        let directory = TempDir::new().expect("a temporary directory");
        let path = directory.path().join("ModsConfig.xml");
        fs::write(&path, document(tokens))
            .unwrap_or_else(|error| panic!("cannot write the config file: {error}"));
        Self { directory, path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn directory(&self) -> &Path {
        self.directory.path()
    }

    /// The bytes the file holds right now.
    fn bytes(&self) -> Vec<u8> {
        fs::read(&self.path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", self.path.display()))
    }

    /// The snapshot those bytes read as.
    fn loaded(&self) -> ConfigSnapshot {
        parse_config(&self.bytes()).expect("the config file must load")
    }

    /// The backup a save leaves beside its target.
    fn backup(&self) -> PathBuf {
        let mut name = self.path.as_os_str().to_os_string();
        name.push(".rimsort-rust.bak");
        PathBuf::from(name)
    }

    /// A session loaded from this file against `catalog`.
    fn session(&self, catalog: Catalog) -> Session {
        Session::new(catalog, self.loaded(), self.path.clone(), version(TARGET))
    }

    /// A session holding one pending edit, so a save has something faithful to write.
    fn edited_session(&self) -> Session {
        let mut session = self.session(catalog_of(vec![
            record(CORE, Source::Official, "ludeon.rimworld"),
            record(HARMONY, Source::Local, "brrainz.harmony"),
        ]));
        session
            .activate(&id(HARMONY))
            .expect("activating an installed copy is encodable");
        session
    }

    /// The settings a workflow is built with: this config file, this basis. Nothing here
    /// points at a real installation.
    fn settings(&self) -> Settings {
        Settings {
            schema_version: 1,
            game_root: Some(self.directory().join("game")),
            content_root: Some(self.directory().join("game/Mods")),
            config_path: Some(self.path().to_path_buf()),
            local_mods_root: Some(self.directory().join("mods")),
            workshop_root: None,
            target_version: Some(version(TARGET)),
            executable_override: None,
            confirm_warnings: false,
        }
    }
}

/// The single `Save` effect a workflow emits for `session`, with the operation identity
/// the effect itself carried.
fn save_effect(workflow: &mut Workflow, session: &mut Session) -> (OperationId, SaveRequest) {
    let effects = workflow.reduce(
        session,
        WorkflowEvent::SaveRequested {
            confirm_warnings: false,
        },
    );
    let mut emitted = effects.into_iter().map(|effect| match effect {
        WorkflowEffect::Save {
            operation,
            path,
            baseline,
            prepared,
        } => (
            operation,
            SaveRequest {
                path,
                baseline,
                prepared,
            },
        ),
        other => panic!("a list this workflow can save must reach a Save effect, got {other:?}"),
    });
    let found = emitted.next().expect("the save must be dispatched");
    assert!(
        emitted.next().is_none(),
        "exactly one write per save request"
    );
    found
}

fn dispatch(
    host: &JobHost,
    owner: JobOwner,
    bound: Option<JobSource>,
    cancel: CancelToken,
    work: JobWork,
) -> JobId {
    host.dispatch(JobRequest {
        owner,
        bound,
        cancel,
        work,
    })
}

/// Joins the job's worker and takes the one completion it produced.
fn delivered(host: &JobHost, current: Option<JobSource>) -> Delivered {
    let panicked = host.reap_workers();
    assert!(
        panicked.is_empty(),
        "no worker of this job may panic: {panicked:?}"
    );
    host.poll_completion(current)
        .expect("a dispatched job must hand back exactly one completion")
}

fn actionable(delivered: Delivered) -> JobCompletion {
    match delivered {
        Delivered::Current(completion) => completion,
        Delivered::Discarded {
            job,
            owner,
            kind,
            bound,
        } => panic!(
            "expected a deliverable result, got a discarded {kind:?} job {job:?} ({owner:?}, bound {bound:?})"
        ),
    }
}

// The refresh job: what one captured candidate has to come back with.

/// A temporary install with a scannable layout, its own config file, and a Workshop
/// root that is not there, so a complete read still has a path-aware problem to carry.
struct Install {
    temp: TempDir,
    settings: Settings,
    config: PathBuf,
}

impl Install {
    fn new() -> Self {
        let temp = TempDir::new().expect("a temporary install");
        let game_root = temp.path().join("game");
        let content_root = game_root.join("Mods");
        let data_root = content_root.join("Data");
        let local_root = temp.path().join("Local Mods");
        let config = temp.path().join("config").join("ModsConfig.xml");
        for directory in [&data_root, &local_root] {
            fs::create_dir_all(directory)
                .unwrap_or_else(|error| panic!("cannot create {}: {error}", directory.display()));
        }
        fs::create_dir_all(config.parent().expect("the config path has a parent"))
            .expect("the config folder must be creatable");
        let settings = Settings {
            schema_version: 1,
            game_root: Some(game_root),
            content_root: Some(content_root),
            config_path: Some(config.clone()),
            local_mods_root: Some(local_root),
            // Deliberately absent: a scan of the rest stays usable and says so.
            workshop_root: Some(temp.path().join("Workshop Absent")),
            target_version: Some(version(TARGET)),
            executable_override: None,
            confirm_warnings: true,
        };
        Self {
            temp,
            settings,
            config,
        }
    }

    /// The resolved root of this fixture, for asserting nothing outside it was read.
    fn root(&self) -> PathBuf {
        self.temp
            .path()
            .canonicalize()
            .unwrap_or_else(|error| panic!("cannot resolve the fixture root: {error}"))
    }

    fn local(&self) -> &Path {
        self.settings
            .local_mods_root
            .as_ref()
            .expect("the local root is configured")
    }

    fn add_mod(&self, name: &str, package_id: &str) {
        let about = self.local().join(name).join("About");
        fs::create_dir_all(&about)
            .unwrap_or_else(|error| panic!("cannot create {about:?}: {error}"));
        fs::write(
            about.join("About.xml"),
            format!(
                "<ModMetaData><packageId>{package_id}</packageId><name>{name}</name>\
                 <supportedVersions><li>1.5</li></supportedVersions></ModMetaData>"
            ),
        )
        .expect("a mod's About.xml must be writable");
    }

    fn write_config(&self, tokens: &[&str]) {
        fs::write(&self.config, document(tokens)).expect("the config file must be writable");
    }

    fn config_bytes(&self) -> Vec<u8> {
        fs::read(&self.config).expect("the config file must be readable")
    }

    fn scanned(&self) -> ScanResult {
        scan_install(&self.settings).expect("this install must scan")
    }

    fn target(&self) -> RefreshTarget {
        RefreshTarget {
            settings: self.settings.clone(),
        }
    }

    /// A session loaded from this install's own scan and config file.
    fn session(&self) -> Session {
        Session::new(
            catalog_of(self.scanned().mods),
            parse_config(&self.config_bytes()).expect("the candidate's config must load"),
            self.config.clone(),
            version(TARGET),
        )
    }
}

#[test]
fn a_refresh_job_reads_the_captured_candidate_and_keeps_its_diagnostics() {
    let install = Install::new();
    install.add_mod("alpha", "alpha.mod");
    install.add_mod("beta", "beta.mod");
    install.write_config(&["alpha.mod"]);
    let session = install.session();
    let bound = JobSource::of(&session);
    let target = install.target();
    let host = JobHost::new();

    let job = dispatch(
        &host,
        JobOwner::Startup,
        Some(bound),
        CancelToken::new(),
        JobWork::Refresh(target.clone()),
    );
    let found = actionable(delivered(&host, Some(bound)));

    assert_eq!(found.job, job, "the result says which job it came from");
    assert_eq!(found.owner, JobOwner::Startup, "and who asked for it");
    assert_eq!(found.kind, JobKind::Refresh);
    assert_eq!(
        found.bound,
        Some(bound),
        "and the session state its input was read at"
    );
    let replacement = match found.outcome {
        JobOutcome::Refreshed(RefreshOutcome::Reloaded(replacement)) => replacement,
        other => panic!("this candidate is readable, so a failure is wrong: {other:?}"),
    };
    assert_eq!(
        replacement.origin, target,
        "the result names the candidate it read"
    );
    assert_eq!(
        replacement.config,
        parse_config(&install.config_bytes()).expect("the candidate's config must load"),
        "the configuration is the candidate's own file, as loaded"
    );
    assert_eq!(
        replacement.catalog.mods().len(),
        2,
        "every installed copy the walk found travels with the result"
    );
    assert_eq!(
        replacement.target_version,
        version(TARGET),
        "and so does the release it was read for"
    );
    let root = install.root();
    assert!(
        replacement
            .catalog
            .mods()
            .iter()
            .all(|record| record.id.as_path().starts_with(&root)),
        "every record the job handed back came out of this fixture, not off a real install"
    );
    assert_eq!(
        replacement.diagnostics,
        install.scanned().diagnostics,
        "the path-aware problems the read met are the scan's own, in the scan's own order"
    );
    assert!(
        !replacement.diagnostics.is_empty(),
        "and this candidate has one to report, not a quiet success"
    );
    assert!(
        host.poll_completion(Some(bound)).is_none(),
        "one dispatched job produces one completion, never a second"
    );
}

#[test]
fn a_refresh_job_that_could_not_read_the_candidate_returns_no_replacement() {
    let install = Install::new();
    install.add_mod("alpha", "alpha.mod");
    // No config file is written: the candidate names a file that is not there.
    let host = JobHost::new();

    dispatch(
        &host,
        JobOwner::Startup,
        None,
        CancelToken::new(),
        JobWork::Refresh(install.target()),
    );
    let found = actionable(delivered(&host, None));

    match found.outcome {
        JobOutcome::Refreshed(RefreshOutcome::Failed(failure)) => assert!(
            failure
                .message
                .contains(&install.config.display().to_string()),
            "the failure names the file it could not read: {}",
            failure.message
        ),
        other => panic!("an unreadable candidate must not produce a replacement: {other:?}"),
    }
}

// Staleness, progress and cancellation at the job boundary.
//
// These cases wait on their own worker by joining it, never on a wall clock and never
// on a filesystem object that could park a thread nobody can wake. A result's
// staleness is decided when it is *delivered*, against the session state the caller
// names then, so the order of work and edit needs no timing assumption at all.
//
// What stops a scan or a sort *in the middle* of its walk is a property of the
// scanner's and sorter's own boundaries, and it is proved there, in `scan.rs` and
// `sorting.rs`, by counting the checks those loops make. A jobs-level case could only
// ask for a stop before or after the work, which is not the same claim, so this suite
// does not make it.

/// An install with enough folders for a walk to pass several boundary checks.
fn walk_install() -> Install {
    let install = Install::new();
    install.add_mod("aaa-first", "first.mod");
    install.add_mod("bbb-second", "second.mod");
    install.add_mod("ccc-third", "third.mod");
    install.add_mod("zzz-last", "last.mod");
    install.write_config(&["first.mod"]);
    install
}

#[test]
fn a_scan_that_finished_after_the_list_changed_is_dropped_and_installs_nothing() {
    let install = walk_install();
    let mut session = install.session();
    let bound = JobSource::of(&session);
    let host = JobHost::new();

    dispatch(
        &host,
        JobOwner::Startup,
        Some(bound),
        CancelToken::new(),
        JobWork::Refresh(install.target()),
    );
    // Join first: the completion is certainly waiting, so what follows is a delivery
    // decision rather than a race about whether the worker had finished.
    assert!(
        host.reap_workers().is_empty(),
        "the scan worker must finish by returning"
    );
    session.deactivate(0).expect("the loaded row is removable");
    assert_eq!(
        JobSource::of(&session),
        JobSource {
            generation: bound.generation,
            revision: bound.revision + 1
        },
        "the edit is what the session looks like now"
    );

    match host
        .poll_completion(Some(JobSource::of(&session)))
        .expect("the finished scan is still there to be classified")
    {
        Delivered::Discarded {
            kind, bound: stale, ..
        } => {
            assert_eq!(kind, JobKind::Refresh, "the dropped work was the scan");
            assert_eq!(
                stale,
                Some(bound),
                "and it was bound to the revision the list had before the edit"
            );
        }
        Delivered::Current(found) => panic!(
            "a scan bound to {:?} was handed back as current for {:?}",
            found.bound,
            JobSource::of(&session)
        ),
    }
    assert!(
        host.poll_completion(Some(JobSource::of(&session)))
            .is_none(),
        "a discarded result is not queued up again behind its own payload"
    );
    assert!(
        session.working_entries().is_empty(),
        "the installation the scan read was never installed over the edit"
    );
}

#[test]
fn progress_hands_the_caller_the_newest_report_and_never_a_backlog_to_drain() {
    let install = walk_install();
    let session = install.session();
    let bound = JobSource::of(&session);
    let host = JobHost::new();

    let job = dispatch(
        &host,
        JobOwner::Startup,
        Some(bound),
        CancelToken::new(),
        JobWork::Refresh(install.target()),
    );
    // The job is over, so every report it will ever publish has been published.
    assert!(
        host.reap_workers().is_empty(),
        "the scan worker must finish by returning"
    );

    let newest: JobProgress = host
        .poll_progress()
        .expect("a finished scan reported its work");
    let behind: Option<JobProgress> = host.poll_progress();

    assert_eq!(newest.job, job, "the report says which job it is about");
    assert_eq!(newest.owner, JobOwner::Startup);
    assert_eq!(newest.kind, JobKind::Refresh);
    assert_eq!(
        (newest.stage, newest.steps),
        (JobStage::BuildingCatalog, 0),
        "the report still held is the newest stage the job entered, so the walk's earlier \
         reports were replaced rather than queued: {newest:?}"
    );
    assert!(
        behind.is_none(),
        "one slot holds one report: a caller that never polled builds no backlog: {behind:?}"
    );
}

#[test]
fn a_scan_asked_to_stop_before_its_work_starts_installs_nothing() {
    let install = walk_install();
    let session = install.session();
    let bound = JobSource::of(&session);
    let host = JobHost::new();
    let cancel = CancelToken::new();
    cancel.cancel();

    let job = dispatch(
        &host,
        JobOwner::Startup,
        Some(bound),
        cancel,
        JobWork::Refresh(install.target()),
    );
    let found = actionable(delivered(&host, Some(bound)));

    assert_eq!(found.job, job);
    assert!(
        matches!(found.outcome, JobOutcome::Cancelled),
        "a scan nobody let start publishes no work product, got {:?}",
        found.outcome
    );
}

// The validation/sort job, and the one gate its order passes through.

#[test]
fn a_sort_job_reports_the_findings_and_the_order_of_the_list_it_read() {
    let config = Config::new(&[CORE_TOKEN, DEPENDENT_TOKEN, PROVIDER_TOKEN]);
    let mut session = config.session(editing_catalog());
    let bound = JobSource::of(&session);
    let findings = session.validate();
    let host = JobHost::new();

    let job = dispatch(
        &host,
        JobOwner::Startup,
        Some(bound),
        CancelToken::new(),
        JobWork::Sort(ModListSnapshot::from_session(&session)),
    );
    let found = actionable(delivered(&host, Some(JobSource::of(&session))));
    assert_eq!(found.job, job);
    assert_eq!(
        found.bound,
        Some(bound),
        "the result says which revision it was read at"
    );
    let report = match &found.outcome {
        JobOutcome::Validated(report) => report,
        other => panic!("this list is sortable, so the job must report it: {other:?}"),
    };
    assert_eq!(
        report.issues, findings,
        "the findings are validation's own for the list it was given"
    );
    let order = report
        .order
        .as_ref()
        .expect("this list has one unambiguous load order");
    assert_eq!(
        tokens_of(order),
        vec![CORE_TOKEN, PROVIDER_TOKEN, DEPENDENT_TOKEN],
        "the copy that must load first comes first, whatever order it was handed in"
    );

    assert_eq!(
        apply_sort_order(&mut session, &found),
        SortApplied::Published,
        "the order reaches the session its input was read from"
    );
    assert_eq!(
        tokens_of(session.working_entries()),
        vec![CORE_TOKEN, PROVIDER_TOKEN, DEPENDENT_TOKEN],
        "and the working list is that order now"
    );
    assert_eq!(
        session.revision(),
        bound.revision + 1,
        "publishing an order is an edit"
    );
    assert_eq!(
        apply_sort_order(&mut session, &found),
        SortApplied::Refused,
        "the same order cannot be published twice: the first publish moved the revision"
    );
}

#[test]
fn a_sort_result_bound_to_an_older_revision_never_reaches_the_working_list() {
    let config = Config::new(&[CORE_TOKEN, DEPENDENT_TOKEN, PROVIDER_TOKEN]);
    let mut session = config.session(editing_catalog());
    let bound = JobSource::of(&session);
    let host = JobHost::new();

    let job = dispatch(
        &host,
        JobOwner::Startup,
        Some(bound),
        CancelToken::new(),
        JobWork::Sort(ModListSnapshot::from_session(&session)),
    );
    // Join the worker first, so the completion is certainly waiting, then edit.
    assert!(
        host.reap_workers().is_empty(),
        "the sort worker must finish by returning"
    );
    session.deactivate(2).expect("the last row is removable");

    match host
        .poll_completion(Some(JobSource::of(&session)))
        .expect("the finished sort result is still there to be classified")
    {
        Delivered::Discarded {
            job: dropped, kind, ..
        } => {
            assert_eq!(dropped, job);
            assert_eq!(kind, JobKind::Sort);
        }
        Delivered::Current(found) => panic!(
            "a sort read at {:?} must not be handed back for {:?}",
            found.bound,
            JobSource::of(&session)
        ),
    }
    assert_eq!(
        tokens_of(session.working_entries()),
        vec![CORE_TOKEN, DEPENDENT_TOKEN],
        "the order the worker computed never overwrote the list the user shortened"
    );
    assert_eq!(
        session.revision(),
        bound.revision + 1,
        "only the user's own edit spent a revision"
    );
}

#[test]
fn a_cancelled_sort_publishes_no_order_and_the_gate_refuses_its_result() {
    let config = Config::new(&[CORE_TOKEN, DEPENDENT_TOKEN, PROVIDER_TOKEN]);
    let mut session = config.session(editing_catalog());
    let bound = JobSource::of(&session);
    let host = JobHost::new();
    let cancel = CancelToken::new();
    cancel.cancel();

    dispatch(
        &host,
        JobOwner::Startup,
        Some(bound),
        cancel,
        JobWork::Sort(ModListSnapshot::from_session(&session)),
    );
    let found = actionable(delivered(&host, Some(bound)));

    assert!(
        matches!(found.outcome, JobOutcome::Cancelled),
        "a sort nobody let start must not report an order: {:?}",
        found.outcome
    );
    assert_eq!(
        apply_sort_order(&mut session, &found),
        SortApplied::Refused,
        "and there is nothing for the publish gate to apply"
    );
    assert_eq!(
        session.revision(),
        bound.revision,
        "the session spent no revision"
    );
    assert_eq!(
        tokens_of(session.working_entries()),
        vec![CORE_TOKEN, DEPENDENT_TOKEN, PROVIDER_TOKEN],
        "and its list is exactly the one the user was looking at"
    );
}

// The save job: a write is a fact, and only the workflow decides whose fact it is.

#[test]
fn a_save_job_writes_the_prepared_document_and_the_workflow_adopts_it_at_its_revision() {
    let config = Config::new(&[CORE_TOKEN]);
    let before = config.bytes();
    let mut session = config.edited_session();
    let mut workflow = Workflow::new(&config.settings(), Platform::Linux, config.directory());
    let bound = JobSource::of(&session);
    let (operation, save) = save_effect(&mut workflow, &mut session);
    assert_eq!(
        save.path,
        config.path(),
        "the effect writes the session's own file"
    );
    let host = JobHost::new();

    dispatch(
        &host,
        JobOwner::Workflow(operation),
        Some(bound),
        CancelToken::new(),
        JobWork::Save(save),
    );
    let found = actionable(delivered(&host, Some(bound)));
    assert_eq!(
        found.owner,
        JobOwner::Workflow(operation),
        "the result echoes the effect's identity"
    );
    assert_eq!(found.kind, JobKind::Save);
    let outcome = match found.outcome {
        JobOutcome::Saved(outcome) => outcome,
        other => panic!("a save job answers for the save, got {other:?}"),
    };
    let committed = match &outcome {
        SaveOutcome::Committed(committed) => committed,
        failed => panic!("the file was there and unchanged, so the write must land: {failed:?}"),
    };
    assert_eq!(
        config.bytes(),
        committed.raw,
        "what landed is exactly the document the job reported"
    );
    assert_eq!(
        committed.active_tokens,
        vec![CORE_TOKEN, HARMONY_TOKEN],
        "and the added row is in it"
    );
    assert_eq!(
        fs::read(config.backup()).expect("a save keeps the bytes it replaced as the backup"),
        before,
        "the accepted backup rule is unchanged by doing the write on a worker"
    );

    let after = workflow.reduce(
        &mut session,
        WorkflowEvent::SaveFinished { operation, outcome },
    );
    assert!(
        after.is_empty(),
        "an adopted save ends the operation: {after:?}"
    );
    assert!(!session.is_dirty(), "the pending edits went with the write");
    assert_eq!(
        session.revision(),
        bound.revision,
        "adopting a save is not an edit"
    );
    assert_eq!(
        session.loaded_config(),
        &config.loaded(),
        "the new baseline is the written bytes"
    );
    assert!(
        host.poll_completion(Some(bound)).is_none(),
        "the write is reported once, and no later cancel turns it into anything else"
    );
}

#[test]
fn a_committed_write_is_still_reported_after_the_list_moved_and_is_never_relabelled_cancelled() {
    let config = Config::new(&[CORE_TOKEN]);
    let mut session = config.edited_session();
    let mut workflow = Workflow::new(&config.settings(), Platform::Linux, config.directory());
    let bound = JobSource::of(&session);
    let (operation, save) = save_effect(&mut workflow, &mut session);
    let host = JobHost::new();
    let cancel = CancelToken::new();

    dispatch(
        &host,
        JobOwner::Workflow(operation),
        Some(bound),
        cancel.clone(),
        JobWork::Save(save),
    );
    // Join first so the write has certainly been attempted, then make a real edit and raise
    // the token after the fact. Both are the attempts to make a landed write look like it
    // did not happen, and neither may relabel the result.
    assert!(
        host.reap_workers().is_empty(),
        "the save worker must finish by returning"
    );
    session.deactivate(0).expect("the first row is removable");
    cancel.cancel();

    // Delivered against the session's CURRENT state, which no longer matches the revision
    // the write was prepared at: a stale fact still has to reach the caller.
    let found = actionable(host.poll_completion(Some(JobSource::of(&session))).expect(
        "a save result is a fact: it is handed on whatever the list looks like now, even \
             when the session has moved past the revision it was prepared at",
    ));
    assert_eq!(
        found.bound,
        Some(bound),
        "and it still says which revision it was prepared against"
    );
    assert!(
        matches!(found.outcome, JobOutcome::Saved(SaveOutcome::Committed(_))),
        "a write that landed is never reported cancelled: {:?}",
        found.outcome
    );
    assert_eq!(
        config.loaded().active_tokens,
        vec![CORE_TOKEN, HARMONY_TOKEN],
        "the bytes the write produced are still on disk"
    );

    let outcome = match found.outcome {
        JobOutcome::Saved(outcome) => outcome,
        other => panic!("expected the save outcome, got {other:?}"),
    };
    let reported = workflow.reduce(
        &mut session,
        WorkflowEvent::SaveFinished { operation, outcome },
    );
    match reported.as_slice() {
        [
            WorkflowEffect::ReportFailure {
                operation: kind,
                message,
            },
        ] => {
            assert_eq!(*kind, OperationKind::Save);
            assert!(
                message.contains("not adopted")
                    && message.contains("keeps the bytes that were written"),
                "the report says what is on disk instead of pretending a rollback: {message}"
            );
        }
        other => panic!("a save the session no longer matches must be reported, got {other:?}"),
    }
    assert!(
        session.is_dirty(),
        "and the edits are still there to save again"
    );
}

#[test]
fn a_save_result_from_another_workflows_operation_adopts_nothing() {
    let mine = Config::new(&[CORE_TOKEN]);
    let theirs = Config::new(&[CORE_TOKEN]);
    let mut session = mine.edited_session();
    let mut workflow = Workflow::new(&mine.settings(), Platform::Linux, mine.directory());
    let bound = JobSource::of(&session);
    let (operation, save) = save_effect(&mut workflow, &mut session);
    let host = JobHost::new();
    dispatch(
        &host,
        JobOwner::Workflow(operation),
        Some(bound),
        CancelToken::new(),
        JobWork::Save(save),
    );
    let found = actionable(delivered(&host, Some(bound)));
    let outcome = match found.outcome {
        JobOutcome::Saved(outcome) => outcome,
        other => panic!("expected the save outcome, got {other:?}"),
    };

    // A different loaded session, in a different workflow, handed this result.
    let mut foreign = theirs.edited_session();
    let mut foreign_workflow =
        Workflow::new(&theirs.settings(), Platform::Linux, theirs.directory());
    let opened = foreign_workflow.reduce(
        &mut foreign,
        WorkflowEvent::CloseRequested {
            confirm_warnings: false,
        },
    );
    assert!(
        !opened.is_empty(),
        "the foreign workflow binds to its own session first"
    );
    let before = foreign.loaded_config().clone();
    let revision = foreign.revision();
    let foreign_bytes = theirs.bytes();

    let effects = foreign_workflow.reduce(
        &mut foreign,
        WorkflowEvent::SaveFinished { operation, outcome },
    );
    assert!(
        effects.is_empty(),
        "a result for somebody else's operation authorizes nothing here: {effects:?}"
    );
    assert_eq!(
        foreign.loaded_config(),
        &before,
        "no foreign bytes become this session's baseline"
    );
    assert_eq!(
        foreign.revision(),
        revision,
        "and no revision is spent on it"
    );
    assert_eq!(
        theirs.bytes(),
        foreign_bytes,
        "nor was the foreign session's own file written"
    );
}

#[test]
fn a_cancelled_save_writes_nothing_and_leaves_no_backup() {
    let config = Config::new(&[CORE_TOKEN]);
    let before = config.bytes();
    let mut session = config.edited_session();
    let mut workflow = Workflow::new(&config.settings(), Platform::Linux, config.directory());
    let bound = JobSource::of(&session);
    let (operation, save) = save_effect(&mut workflow, &mut session);
    let host = JobHost::new();
    let cancel = CancelToken::new();
    cancel.cancel();

    dispatch(
        &host,
        JobOwner::Workflow(operation),
        Some(bound),
        cancel,
        JobWork::Save(save),
    );
    let found = actionable(delivered(&host, Some(bound)));

    assert!(
        matches!(found.outcome, JobOutcome::Cancelled),
        "a save nobody let run must not write: {:?}",
        found.outcome
    );
    assert_eq!(
        config.bytes(),
        before,
        "the file still holds the bytes it was loaded from"
    );
    assert!(
        !config.backup().exists(),
        "and no backup was made for a write that never happened"
    );
}

#[test]
fn a_save_job_that_found_the_file_changed_reports_the_conflict_and_keeps_the_foreign_bytes() {
    let config = Config::new(&[CORE_TOKEN]);
    let mut session = config.edited_session();
    let mut workflow = Workflow::new(&config.settings(), Platform::Linux, config.directory());
    let bound = JobSource::of(&session);
    let (operation, save) = save_effect(&mut workflow, &mut session);
    // Somebody else writes the file between the effect and the worker.
    fs::write(config.path(), document(&[CORE_TOKEN, DEPENDENT_TOKEN]))
        .expect("the rival write must land");
    let rival = config.bytes();
    let host = JobHost::new();

    dispatch(
        &host,
        JobOwner::Workflow(operation),
        Some(bound),
        CancelToken::new(),
        JobWork::Save(save),
    );
    let found = actionable(delivered(&host, Some(bound)));
    match found.outcome {
        JobOutcome::Saved(SaveOutcome::Failed(failure)) => assert!(
            failure.message.contains("changed since it was loaded"),
            "the conflict is the loader's own refusal: {}",
            failure.message
        ),
        other => panic!("a moved baseline must not be written over: {other:?}"),
    }
    assert_eq!(
        config.bytes(),
        rival,
        "the other writer's bytes are untouched"
    );
}

// The launch job: exactly the command it was handed, and nothing else.

/// A harmless fixture that reports how many arguments it got and what the first one
/// was, so argument boundaries are proven rather than assumed.
#[cfg(unix)]
fn argv_fixture(directory: &Path) -> LaunchCommand {
    let script = directory.join("record-argv.sh");
    let marker = directory.join("argv");
    let text = format!(
        "#!/bin/sh\nprintf '%s|%s' \"$#\" \"$1\" > '{}'\n",
        marker.display()
    );
    fs::write(&script, text).expect("the fixture script must be writable");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755))
        .expect("the fixture script must be executable");
    LaunchCommand {
        program: script,
        args: vec!["one argument with spaces".into()],
        working_directory: Some(directory.to_path_buf()),
    }
}

#[test]
#[cfg(unix)]
fn a_launch_job_starts_the_exact_command_it_was_handed_and_reports_the_process_it_accepted() {
    let directory = TempDir::new().expect("a temporary directory");
    let command = argv_fixture(directory.path());
    let marker = directory.path().join("argv");
    let host = JobHost::new();

    let job = dispatch(
        &host,
        JobOwner::Startup,
        None,
        CancelToken::new(),
        JobWork::Launch(command.clone()),
    );
    let found = actionable(delivered(&host, None));
    assert_eq!(found.job, job);
    assert_eq!(found.kind, JobKind::Launch);
    assert_eq!(found.owner, JobOwner::Startup);
    let process_id = match found.outcome {
        JobOutcome::Launched(LaunchOutcome::Spawned { process_id }) => process_id,
        other => panic!("the fixture is executable, so the start must be accepted: {other:?}"),
    };
    assert_ne!(
        process_id,
        std::process::id(),
        "the accepted process is another one"
    );

    // A cleanup watchdog, not a measurement: it bounds what a fixture that never ran can
    // cost, and the assertion below is about the content the fixture wrote.
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && !marker.exists() {
        std::thread::sleep(Duration::from_millis(20));
    }
    let recorded = fs::read_to_string(&marker).unwrap_or_else(|error| {
        panic!("the launched fixture must have recorded its argv: {error}")
    });
    assert_eq!(
        recorded, "1|one argument with spaces",
        "one argument, exactly its own text, never rejoined into a command line"
    );
}

#[test]
fn a_launch_job_the_operating_system_refused_names_the_program_it_tried() {
    let directory = TempDir::new().expect("a temporary directory");
    let missing = directory.path().join("not-a-program");
    let host = JobHost::new();

    dispatch(
        &host,
        JobOwner::Startup,
        None,
        CancelToken::new(),
        JobWork::Launch(LaunchCommand {
            program: missing.clone(),
            args: Vec::new(),
            working_directory: None,
        }),
    );
    let found = actionable(delivered(&host, None));

    match found.outcome {
        JobOutcome::Launched(LaunchOutcome::Failed(failure)) => assert!(
            failure.message.contains(&missing.display().to_string()),
            "the refusal names the program it could not start: {}",
            failure.message
        ),
        other => panic!("a program that is not there cannot be accepted: {other:?}"),
    }
}

// Shutdown: a worker that finishes too late must not be anybody's problem.

#[test]
fn a_late_result_after_shutdown_is_dropped_and_its_worker_returns_without_panicking() {
    let install = walk_install();
    let session = install.session();
    let bound = JobSource::of(&session);
    let host = JobHost::new();

    // A job already running when the caller stops listening, and one dispatched after
    // the seal, whose worker can only post too late. Both are the shutdown race, and
    // neither is allowed to be observable except as nothing arriving.
    dispatch(
        &host,
        JobOwner::Startup,
        Some(bound),
        CancelToken::new(),
        JobWork::Refresh(install.target()),
    );
    host.shutdown();
    assert!(host.sealed(), "the caller has stopped listening");
    dispatch(
        &host,
        JobOwner::Startup,
        Some(bound),
        CancelToken::new(),
        JobWork::Sort(ModListSnapshot::from_session(&install.session())),
    );

    // Joining proves both workers reached the end of their work and posted into a host
    // that no longer reads: a panic would come back here as a named job.
    let panicked = host.reap_workers();
    assert!(
        panicked.is_empty(),
        "a result that arrived too late must not panic a worker"
    );
    assert!(
        host.discarded_results() >= 1,
        "the job dispatched after the seal can only have finished too late, and its result \
         must be accounted for as dropped rather than vanish: {} discarded",
        host.discarded_results()
    );
    assert!(
        host.poll_completion(Some(bound)).is_none(),
        "a sealed host hands nothing back, whatever a worker posted"
    );
    assert!(
        host.poll_progress().is_none(),
        "and no progress report either"
    );
    assert!(
        host.poll_completion(None).is_none(),
        "not even to a caller that asks without a session state"
    );
}

/// A real job identity for a synthetic completion: taken from a job this suite actually
/// dispatched and drained, so no test invents an id the host never handed out.
fn drained_job_id(host: &JobHost, session: &Session, bound: JobSource) -> JobId {
    let cancel = CancelToken::new();
    cancel.cancel();
    let job = dispatch(
        host,
        JobOwner::Startup,
        Some(bound),
        cancel,
        JobWork::Sort(ModListSnapshot::from_session(session)),
    );
    let found = actionable(delivered(host, Some(bound)));
    assert_eq!(
        found.job, job,
        "the drained completion is the job this suite dispatched"
    );
    assert!(
        matches!(found.outcome, JobOutcome::Cancelled),
        "the drain job is deliberately cancelled, so it changes nothing: {:?}",
        found.outcome
    );
    job
}

// The publication gate refuses lists the sorter itself would refuse.

/// A copy of one package id under two different raw spellings, so a session can hold the
/// same physical copy twice and a candidate order can ask to trade those rows. The two
/// tokens normalize alike, which is exactly why the active list can carry one copy twice.
fn ambiguous_catalog() -> Catalog {
    catalog_of(vec![
        record(CORE, Source::Official, "ludeon.rimworld"),
        record("/mods/twice", Source::Local, "twice.mod"),
    ])
}

#[test]
fn a_completion_the_sorter_would_have_refused_is_not_published() {
    // Two rows naming one physical copy under two spellings: the accepted sorter refuses a
    // repeated identity, so the publication gate has to refuse an order that asks to swap
    // them too, rather than map provenance by copy identity and hand one row's history to
    // both. This is a completion built at the gate's own public boundary, which is the
    // surface 6B will hand a worker's report through.
    let config = Config::new(&["twice.mod", "Twice.Mod"]);
    let mut session = config.session(ambiguous_catalog());
    let rows = session.working_entries().to_vec();
    assert_eq!(
        rows.iter().filter_map(ActiveEntry::mod_id).count(),
        2,
        "both spellings resolved, so the session holds two resolved rows"
    );
    assert_eq!(
        rows[0].mod_id(),
        rows[1].mod_id(),
        "and this fixture really is one copy named twice, which the sorter will not take"
    );
    let bound = JobSource::of(&session);
    let before = session.loaded_config().clone();
    let revision = session.revision();
    let dirty = session.is_dirty();
    let mut swapped = rows.clone();
    swapped.swap(0, 1);
    let host = JobHost::new();
    let job = drained_job_id(&host, &session, bound);

    let completion = JobCompletion {
        job,
        owner: JobOwner::Startup,
        kind: JobKind::Sort,
        bound: Some(bound),
        outcome: rimsort_rust::application::jobs::JobOutcome::Validated(SortReport {
            issues: Vec::new(),
            order: Ok(swapped),
        }),
    };

    assert_eq!(
        apply_sort_order(&mut session, &completion),
        SortApplied::Refused,
        "a repeated physical copy is not something the gate may reorder"
    );
    assert_eq!(
        tokens_of(session.working_entries()),
        tokens_of(&rows),
        "the list is exactly the rows it held"
    );
    assert_eq!(session.revision(), revision, "no revision was spent");
    assert_eq!(session.is_dirty(), dirty, "the dirty state did not move");
    assert_eq!(
        session.loaded_config(),
        &before,
        "and the persistence baseline is untouched by a refused order"
    );
}

#[test]
fn a_completion_holding_an_unresolved_row_is_not_published_even_with_the_same_token() {
    // An unresolved placeholder is not a row an order can carry: the reason is part of what
    // the row says, so an order that keeps the token while changing the reason is not the
    // list this session holds, and the accepted sorter refuses unresolved entries anyway.
    let config = Config::new(&[CORE_TOKEN, "Not.Installed"]);
    let mut session = config.session(ambiguous_catalog());
    let bound = JobSource::of(&session);
    let revision = session.revision();
    let mut candidate = session.working_entries().to_vec();
    candidate[1] = ActiveEntry::Unresolved {
        original_token: "Not.Installed".to_string(),
        reason: rimsort_rust::domain::catalog::UnresolvedReason::EmptyToken,
    };
    let host = JobHost::new();
    let job = drained_job_id(&host, &session, bound);

    let completion = JobCompletion {
        job,
        owner: JobOwner::Startup,
        kind: JobKind::Sort,
        bound: Some(bound),
        outcome: rimsort_rust::application::jobs::JobOutcome::Validated(SortReport {
            issues: Vec::new(),
            order: Ok(candidate),
        }),
    };

    assert_eq!(
        apply_sort_order(&mut session, &completion),
        SortApplied::Refused,
        "the gate refuses rather than adopt a row whose own reason changed"
    );
    assert_eq!(
        session.revision(),
        revision,
        "and the refusal costs nothing"
    );
    assert_eq!(
        tokens_of(session.working_entries()),
        vec![CORE_TOKEN, "Not.Installed"],
        "the placeholder keeps the text and the reason it was loaded with"
    );
}

#[test]
fn a_cancelled_completion_never_publishes_an_order() {
    // The other half of the same boundary: a job that stopped publishes nothing, so the
    // gate has no order to consider and must leave the session alone rather than treat a
    // cancellation as an empty order.
    let config = Config::new(&[CORE_TOKEN, DEPENDENT_TOKEN, PROVIDER_TOKEN]);
    let mut session = config.session(editing_catalog());
    let bound = JobSource::of(&session);

    let host = JobHost::new();
    let job = dispatch(
        &host,
        JobOwner::Startup,
        Some(bound),
        {
            let cancel = CancelToken::new();
            cancel.cancel();
            cancel
        },
        JobWork::Sort(ModListSnapshot::from_session(&session)),
    );
    let cancelled = actionable(delivered(&host, Some(bound)));
    assert!(
        matches!(cancelled.outcome, JobOutcome::Cancelled),
        "the host itself must report the cancelled sort, not an order"
    );

    let completion = JobCompletion {
        job,
        owner: JobOwner::Startup,
        kind: JobKind::Sort,
        bound: Some(bound),
        outcome: rimsort_rust::application::jobs::JobOutcome::Cancelled,
    };

    assert_eq!(
        apply_sort_order(&mut session, &completion),
        SortApplied::Refused,
        "a cancelled job has nothing to publish"
    );
    assert_eq!(session.revision(), bound.revision, "and nothing was spent");
    assert_eq!(
        tokens_of(session.working_entries()),
        vec![CORE_TOKEN, DEPENDENT_TOKEN, PROVIDER_TOKEN],
        "the list the user was looking at is still the list"
    );
}

#[test]
fn a_write_or_launch_slot_never_retires_the_job_whose_fact_is_still_expected() {
    // The window offers cancellation for a scan or a sort, never for a write it is
    // waiting for. A second dispatch on a fact slot is therefore not a silent
    // replacement: the job whose completion the workflow still owes an answer for stays
    // the slot's job, and the second request is refused rather than dropping the first.
    let config = Config::new(&[CORE_TOKEN]);
    let mut session = config.edited_session();
    let mut workflow = Workflow::new(&config.settings(), Platform::Linux, config.directory());
    let bound = JobSource::of(&session);
    let (operation, save) = save_effect(&mut workflow, &mut session);
    let host = JobHost::new();
    let mut jobs = ComputeJobs::new();

    let first = jobs
        .dispatch_fact(
            &host,
            JobSlot::Save,
            operation,
            bound,
            JobWork::Save(save.clone()),
        )
        .expect("the first write is accepted");
    let again = jobs.dispatch_fact(
        &host,
        JobSlot::Save,
        operation,
        JobSource::of(&session),
        JobWork::Save(save),
    );
    assert_eq!(
        again,
        Err(Busy {
            slot: JobSlot::Save,
            job: first
        }),
        "a write the window is still waiting for is never displaced by a second one"
    );
    assert_eq!(
        jobs.active(JobSlot::Save),
        Some(first),
        "and the outstanding job is still the one whose fact must be routed"
    );
    assert!(
        jobs.cancel(JobSlot::Save).is_none(),
        "cancellation is not offered for a fact slot"
    );

    let found = actionable(delivered(&host, Some(bound)));
    assert_eq!(
        jobs.route(&found, Some(bound)),
        Routed::Accepted(JobKind::Save),
        "the write's own result is still delivered to the workflow"
    );
}

#[test]
fn a_completion_bound_to_another_session_state_is_not_routed_even_by_its_own_slot() {
    // Both gates are needed: a job that is still the slot's newest one but was computed
    // against a session state that has moved on cannot act either.
    let install = walk_install();
    let mut session = install.session();
    let bound = JobSource::of(&session);
    let host = JobHost::new();
    let mut jobs = ComputeJobs::new();
    let job = jobs.dispatch_compute(
        &host,
        JobSlot::Sort,
        JobOwner::Startup,
        Some(bound),
        JobWork::Sort(ModListSnapshot::from_session(&session)),
    );
    let found = actionable(delivered(&host, Some(bound)));
    assert_eq!(found.job, job);

    session
        .deactivate(0)
        .expect("an edit moves the session state on");

    assert_eq!(
        jobs.route(&found, Some(JobSource::of(&session))),
        Routed::Superseded,
        "the newest job of the slot is still not a result for a state that no longer exists"
    );
}

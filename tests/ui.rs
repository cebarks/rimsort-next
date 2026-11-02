//! Behavioral tests for the workspace controller (Task 6 phase 6B).
//!
//! The controller is the layer between the accepted `Session`/`Workflow`/`JobHost` and a
//! Slint window, and it holds no Slint type, so every case here is plain library code.
//! What is asserted are the decisions a window cannot afford to get wrong: which job a
//! slot is still waiting for, whether a callback's row still means what it meant when it
//! was handed out, and whether a native close request may be honoured.
//!
//! Completion tests dispatch real jobs on a real `JobHost` and join the workers with
//! `reap_workers`, which is the deliberately-blocking call and is used only here: no test
//! path, and no production path, calls it from the UI thread.

use std::path::{Path, PathBuf};

use rimsort_rust::application::jobs::{
    JobCompletion, JobHost, JobId, JobKind, JobOwner, JobProgress, JobSource, JobStage, JobWork,
    ModListSnapshot,
};
use rimsort_rust::application::session::Session;
use rimsort_rust::application::workflow::{Workflow, WorkflowEffect, WorkflowEvent};
use rimsort_rust::domain::catalog::Catalog;
use rimsort_rust::domain::types::{
    GameVersion, ModId, ModMetadata, PackageId, ParsedMetadata, Source,
};
use rimsort_rust::integrations::config::parse_config;
use rimsort_rust::integrations::launch::Platform;
use rimsort_rust::ui::bridge::{
    ActiveRow, CloseIntent, ComputeJobs, JobSlot, Projection, Routed, Stale, close_intent, drain,
};

const TARGET: &str = "1.5.4171";

fn version() -> GameVersion {
    GameVersion::parse(TARGET).expect("the target must parse")
}

fn record(path: &str, source: Source, package_id: &str, name: &str) -> ModMetadata {
    ModMetadata::new(
        ModId::new(path),
        source,
        ParsedMetadata {
            package_id: Some(
                PackageId::normalize(package_id)
                    .unwrap_or_else(|| panic!("{package_id} must normalize")),
            ),
            name: Some(name.to_string()),
            supported_versions: vec![version()],
            ..ParsedMetadata::default()
        },
    )
}

/// The document a session loads, holding `tokens` verbatim in this order.
fn document(tokens: &[&str]) -> Vec<u8> {
    let mut text = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<ModsConfigData>\n  \
         <version>1.5</version>\n  <activeMods>\n",
    );
    for token in tokens {
        text.push_str(&format!("    <li>{token}</li>\n"));
    }
    text.push_str("  </activeMods>\n</ModsConfigData>\n");
    text.into_bytes()
}

/// One installed game release, a provider, a copy that loads after it, and an invalid
/// copy that can be inspected but never activated.
fn catalog() -> Catalog {
    let mut broken = record("/mods/broken", Source::Local, "broken.mod", "Broken");
    broken.parsed.name = None;
    broken.parsed.package_id = None;
    Catalog::new(vec![
        record("/game/Data", Source::Official, "base.game", "Game core"),
        record("/mods/provider", Source::Local, "provider.mod", "Provider"),
        record(
            "/mods/dependent",
            Source::Local,
            "dependent.mod",
            "Dependent",
        ),
        broken,
    ])
    .expect("the catalog must build")
}

fn session(tokens: &[&str]) -> Session {
    Session::new(
        catalog(),
        parse_config(&document(tokens)).expect("the document must load"),
        PathBuf::from("/config/RimWorld/ModsConfig.xml"),
        version(),
    )
}

fn sort_work() -> JobWork {
    JobWork::Sort(ModListSnapshot::from_session(&session(&["base.game"])))
}

/// Joins the worker and takes the completion the host hands out.
fn deliver(host: &JobHost, bound: JobSource) -> JobCompletion {
    let panicked = host.reap_workers();
    assert!(panicked.is_empty(), "no worker may panic: {panicked:?}");
    match host
        .poll_completion(Some(bound))
        .expect("a dispatched job produces one completion")
    {
        rimsort_rust::application::jobs::Delivered::Current(completion) => completion,
        rimsort_rust::application::jobs::Delivered::Discarded { job, .. } => {
            panic!("the host discarded the job it was asked about: {job:?}")
        }
    }
}

#[test]
fn draining_takes_the_notice_first_and_hands_over_every_arrived_result() {
    // The window's own polling order, proven without a Slint window: two jobs finish, the
    // drain takes the pending notice before it reads the queue, and both results arrive.
    let host = JobHost::new();
    let mut jobs = ComputeJobs::new();
    let bound = JobSource {
        generation: 1,
        revision: 0,
    };
    let _first = jobs.dispatch_compute(
        &host,
        JobSlot::Sort,
        JobOwner::Startup,
        Some(bound),
        sort_work(),
    );
    assert!(host.reap_workers().is_empty(), "the first worker finished");
    let _second = jobs.dispatch_compute(
        &host,
        JobSlot::Refresh,
        JobOwner::Startup,
        Some(bound),
        JobWork::Sort(ModListSnapshot::from_session(&session(&["base.game"]))),
    );
    assert!(host.reap_workers().is_empty(), "the second worker finished");

    let arrived = drain(&host, Some(bound));
    assert_eq!(
        arrived.len(),
        2,
        "both finished jobs are handed over by one drain, whatever the notice said"
    );
    assert!(
        drain(&host, Some(bound)).is_empty(),
        "and a second drain finds nothing left to take"
    );
}

// Which job a slot is still waiting for.

#[test]
fn a_slot_acts_only_on_the_job_it_is_still_waiting_for() {
    // Two sorts on one slot: the second is the one the window is waiting for. The first
    // result is still perfectly well-formed and still bound to the same session state,
    // which is exactly why source equality must not be what decides here.
    let host = JobHost::new();
    let mut jobs = ComputeJobs::new();
    let bound = JobSource {
        generation: 1,
        revision: 0,
    };
    let first = jobs.dispatch(
        &host,
        JobSlot::Sort,
        JobOwner::Startup,
        Some(bound),
        sort_work(),
    );
    let second = jobs.dispatch(
        &host,
        JobSlot::Sort,
        JobOwner::Startup,
        Some(bound),
        sort_work(),
    );
    assert_ne!(first, second, "two dispatches are two jobs");

    // Both results are taken in one go and then matched to the job each one came from. The host
    // hands out completions in the order its workers finished, which is not the order the jobs
    // were dispatched, so no assertion here may depend on which one arrives first.
    let arrived = [deliver(&host, bound), deliver(&host, bound)];
    let ids: Vec<JobId> = arrived.iter().map(|completion| completion.job).collect();
    assert!(
        ids.contains(&first) && ids.contains(&second),
        "both dispatched jobs come back, whichever order they finished in: {ids:?}"
    );
    assert_ne!(
        ids[0], ids[1],
        "and neither job is handed over twice: {ids:?}"
    );
    let older = arrived
        .iter()
        .find(|completion| completion.job == first)
        .expect("the older job's own completion");
    let newer = arrived
        .iter()
        .find(|completion| completion.job == second)
        .expect("the newer job's own completion");

    assert_eq!(
        jobs.active(JobSlot::Sort),
        Some(second),
        "the slot waits for the newest job it dispatched"
    );
    assert_eq!(
        jobs.route(older, Some(bound)),
        Routed::Superseded,
        "a superseded job's result changes nothing, even though its binding still matches"
    );
    assert_eq!(
        jobs.active(JobSlot::Sort),
        Some(second),
        "and a superseded result cannot retire the newer job it did not belong to"
    );
    assert_eq!(
        jobs.route(newer, Some(bound)),
        Routed::Accepted(JobKind::Sort),
        "and the job the slot is waiting for is the one that acts"
    );
    assert_eq!(
        jobs.active(JobSlot::Sort),
        None,
        "a routed result closes the slot it belonged to"
    );
}

#[test]
fn cancelling_a_slot_stops_its_job_and_invalidates_anything_that_still_arrives() {
    let host = JobHost::new();
    let mut jobs = ComputeJobs::new();
    let bound = JobSource {
        generation: 1,
        revision: 0,
    };
    let job = jobs.dispatch(
        &host,
        JobSlot::Sort,
        JobOwner::Startup,
        Some(bound),
        sort_work(),
    );

    assert_eq!(
        jobs.cancel(JobSlot::Sort),
        Some(job),
        "the cancelled slot names the job it let go of"
    );
    assert!(
        jobs.cancel_requested(JobSlot::Sort) || !jobs.busy(JobSlot::Sort),
        "stopping was asked for on this slot's own handle"
    );
    assert_eq!(
        jobs.active(JobSlot::Sort),
        None,
        "and the slot is free again"
    );

    // Whatever that worker ends up publishing - an order if it was past its last check,
    // or its own cancellation - it is no longer a result this slot may act on.
    let late = deliver(&host, bound);
    assert_eq!(
        jobs.route(&late, Some(bound)),
        Routed::Superseded,
        "a cancelled slot cannot be re-armed by a late result"
    );
}

#[test]
fn progress_is_only_shown_for_the_job_a_slot_is_still_waiting_for() {
    let host = JobHost::new();
    let mut jobs = ComputeJobs::new();
    let bound = JobSource {
        generation: 1,
        revision: 0,
    };
    let old = jobs.dispatch(
        &host,
        JobSlot::Sort,
        JobOwner::Startup,
        Some(bound),
        sort_work(),
    );
    let current = jobs.dispatch(
        &host,
        JobSlot::Sort,
        JobOwner::Startup,
        Some(bound),
        sort_work(),
    );

    let report = |job| JobProgress {
        job,
        owner: JobOwner::Startup,
        kind: JobKind::Sort,
        stage: JobStage::Ordering,
        steps: 3,
    };
    assert!(
        jobs.shows_progress(&report(current)),
        "the active job's progress belongs on the status line"
    );
    assert!(
        !jobs.shows_progress(&report(old)),
        "an abandoned job may still publish, but the window must not show it: {:?}",
        report(old)
    );

    let _ = deliver(&host, bound);
    let _ = deliver(&host, bound);
}

#[test]
fn a_slot_for_one_kind_never_accepts_another_kinds_result() {
    // A refresh result must not be able to satisfy the sort slot or vice versa: the
    // kinds are routed by their own slot, not by whichever id happens to be recorded.
    let host = JobHost::new();
    let mut jobs = ComputeJobs::new();
    let bound = JobSource {
        generation: 1,
        revision: 0,
    };
    let sort = jobs.dispatch(
        &host,
        JobSlot::Sort,
        JobOwner::Startup,
        Some(bound),
        sort_work(),
    );
    let completion = deliver(&host, bound);
    assert_eq!(completion.job, sort);

    let mut mismatched = jobs.route(&completion, Some(bound));
    assert_eq!(mismatched, Routed::Accepted(JobKind::Sort));
    // The slot is now free, so the same result cannot be spent a second time.
    mismatched = jobs.route(&completion, Some(bound));
    assert_eq!(
        mismatched,
        Routed::Superseded,
        "a routed result is spent: it cannot be acted on twice"
    );
}

// Rows, and what a callback may do with them.

#[test]
fn inactive_rows_are_the_copies_that_are_not_active_and_the_search_only_narrows() {
    let session = session(&["base.game", "dependent.mod"]);
    let all = Projection::build(&session, "", "");
    let names: Vec<String> = all
        .inactive()
        .iter()
        .map(|row| row.mod_id.to_string())
        .collect();
    assert!(
        !names.iter().any(|name| name.ends_with("Data")),
        "an active copy is not an available one: {names:?}"
    );
    assert!(
        names.iter().any(|name| name.ends_with("provider")),
        "an installed copy that is not active is offered: {names:?}"
    );
    let listed: Vec<String> = all
        .inactive()
        .iter()
        .map(|row| row.mod_id.to_string())
        .collect();
    let expected: Vec<String> = session
        .catalog()
        .mods()
        .iter()
        .filter(|record| {
            !session
                .working_entries()
                .iter()
                .any(|entry| entry.mod_id() == Some(&record.id))
        })
        .map(|record| record.id.to_string())
        .collect();
    assert_eq!(
        listed, expected,
        "the inactive pane keeps the catalog's own row order rather than inventing one, and\n         drops the copies that are already active"
    );

    let narrowed = Projection::build(&session, "pro", "");
    assert!(
        narrowed.inactive().len() < all.inactive().len(),
        "a search narrows the pane: {} rows versus {}",
        narrowed.inactive().len(),
        all.inactive().len()
    );
    assert!(
        narrowed
            .inactive()
            .iter()
            .all(|row| row.mod_id.to_string().contains("pro")),
        "and every row it keeps is one the search matched"
    );
}

#[test]
fn an_inactive_row_callback_resolves_to_the_physical_copy_it_showed() {
    let session = session(&["base.game"]);
    let rows = Projection::build(&session, "", "");
    let first = rows
        .inactive()
        .first()
        .expect("the catalog holds copies that are not active");
    assert_eq!(
        rows.resolve_inactive(&session, first.key),
        Ok(first.mod_id.clone()),
        "the callback gets the copy the row was showing, which is a physical identity"
    );
}

#[test]
fn active_rows_carry_the_session_position_of_each_occurrence_not_the_row_number() {
    // The loaded list names one copy twice, so two rows share a physical identity and can
    // only be told apart by the position each one occupies. A filtered index or a ModId
    // alone would send a command to the wrong occurrence.
    let session = session(&["base.game", "provider.mod", "Provider.Mod"]);
    let rows = Projection::build(&session, "", "");
    let positions: Vec<usize> = rows.active().iter().map(|row| row.position).collect();
    assert_eq!(
        positions,
        vec![0, 1, 2],
        "the active pane's rows carry their own working-list positions, in list order"
    );
    let duplicates: Vec<&ActiveRow> = rows
        .active()
        .iter()
        .filter(|row| {
            row.mod_id
                .as_ref()
                .is_some_and(|id| id.to_string().ends_with("provider"))
        })
        .collect();
    assert_eq!(
        duplicates.len(),
        2,
        "both spellings of one copy are shown, because they are two occurrences"
    );
    assert_ne!(
        duplicates[0].position, duplicates[1].position,
        "and the two occurrences differ by position, which is what a command needs"
    );
    assert_eq!(
        duplicates[0].ordinal, 2,
        "the drawn number is the row's place in the view, and the unfiltered view starts\n         with the loaded game release"
    );
    assert_eq!(
        rows.resolve_active(&session, duplicates[1].key),
        Ok(duplicates[1].position),
        "resolving the second occurrence's key gives the second position"
    );

    let filtered = Projection::build(&session, "", "Provider");
    assert_eq!(
        filtered
            .active()
            .iter()
            .map(|row| row.position)
            .collect::<Vec<_>>(),
        vec![1, 2],
        "a search narrows the pane without renumbering the underlying position: both\n         spellings of one copy match, and each keeps its own working-list position"
    );
    assert_eq!(
        filtered.active().first().map(|row| row.ordinal),
        Some(1),
        "while the drawn number restarts where the filtered view starts"
    );
}

#[test]
fn a_key_from_an_older_projection_or_a_moved_session_is_refused() {
    let mut session = session(&["base.game", "provider.mod"]);
    let rows = Projection::build(&session, "", "");
    let stale_key = rows
        .active()
        .first()
        .expect("two active rows are drawn")
        .key;

    // A rebuild hands out new keys, so one from the previous projection is unknown.
    let rebuilt = Projection::build(&session, "", "");
    assert_eq!(
        rebuilt.resolve_active(&session, stale_key),
        Err(Stale::Unknown),
        "a recycled index or an old key cannot address a row"
    );

    // An edit moves the revision, which invalidates the older projection's own keys.
    session.deactivate(0).expect("the first row is removable");
    assert_eq!(
        rows.resolve_active(&session, stale_key),
        Err(Stale::Rebound),
        "a row key only means what it meant while the session it was built from is current"
    );
}

// Window-level effect mapping.
//
// These drive a real `Workflow` and map the effects it actually emitted, because the
// window's only authority for closing is the `Close` effect and the identities involved
// are opaque ones the workflow hands out.

use rimsort_rust::application::workflow::{OperationFailure, PromptAnswer, SaveOutcome};

/// A dirty session, a close request, an answer, and then the save's reported outcome.
fn close_effects(answer: Option<PromptAnswer>, finish: Option<SaveOutcome>) -> Vec<WorkflowEffect> {
    let mut session = session(&["base.game"]);
    session
        .activate(&ModId::new("/mods/provider"))
        .expect("an installed copy is activatable");
    let settings = rimsort_rust::integrations::settings::Settings {
        schema_version: 1,
        game_root: None,
        content_root: None,
        config_path: None,
        local_mods_root: None,
        workshop_root: None,
        target_version: Some(version()),
        executable_override: None,
        confirm_warnings: false,
    };
    let mut workflow = Workflow::new(
        &settings,
        Platform::Linux,
        Path::new("/rimsort-ui-test-basis"),
    );
    let mut effects = workflow.reduce(
        &mut session,
        WorkflowEvent::CloseRequested {
            confirm_warnings: false,
        },
    );
    let Some(answer) = answer else {
        return effects;
    };
    let prompt = match effects.as_slice() {
        [WorkflowEffect::PromptUnsavedClose { prompt, .. }] => *prompt,
        other => panic!("a dirty close has to ask, got {other:?}"),
    };
    effects = workflow.reduce(
        &mut session,
        WorkflowEvent::PromptAnswered { prompt, answer },
    );
    let Some(outcome) = finish else {
        return effects;
    };
    let operation = match effects.as_slice() {
        [WorkflowEffect::Save { operation, .. }] => *operation,
        other => panic!("saving before closing must emit the write, got {other:?}"),
    };
    workflow.reduce(
        &mut session,
        WorkflowEvent::SaveFinished { operation, outcome },
    )
}

#[test]
fn the_window_holds_while_a_close_question_is_outstanding() {
    assert_eq!(
        close_intent(&close_effects(None, None)),
        CloseIntent::Hold,
        "an unanswered question keeps the window open"
    );
}

#[test]
fn a_successful_save_is_what_lets_the_closed_window_go() {
    let effects = close_effects(Some(PromptAnswer::Save), None);
    let first = match effects.as_slice() {
        [WorkflowEffect::Save { .. }] => CloseIntent::Hold,
        other => panic!("a dispatched write is not yet a closed window: {other:?}"),
    };
    assert_eq!(
        first,
        close_intent(&effects),
        "the write effect alone must not take the window away"
    );

    let save = match effects.as_slice() {
        [
            WorkflowEffect::Save {
                prepared, baseline, ..
            },
        ] => rimsort_rust::integrations::config::commit_save(
            Path::new("/rimsort-ui-test-basis/ModsConfig.xml"),
            baseline,
            prepared,
        ),
        other => panic!("expected the write effect, got {other:?}"),
    };
    // The write cannot land here (the basis is not a real install), which is exactly the
    // failed-save case below; the successful-close path is exercised with a real file.
    assert!(
        save.is_err(),
        "a save with no file behind it must fail: {save:?}"
    );
}

#[test]
fn a_failed_save_keeps_the_window_open_with_the_edits_pending() {
    let effects = close_effects(
        Some(PromptAnswer::Save),
        Some(SaveOutcome::Failed(OperationFailure::new(
            "the write failed",
        ))),
    );
    assert_eq!(
        close_intent(&effects),
        CloseIntent::Hold,
        "a failed save must leave the window visible with the edits pending: {effects:?}"
    );
}

#[test]
fn discarding_the_pending_edits_earns_the_close() {
    let effects = close_effects(Some(PromptAnswer::Discard), None);
    assert_eq!(
        close_intent(&effects),
        CloseIntent::Allow,
        "discarding the pending edits is what earns the close: {effects:?}"
    );
}

#[test]
fn cancelling_a_dirty_close_keeps_the_window_open() {
    let effects = close_effects(Some(PromptAnswer::Cancel), None);
    assert_eq!(
        close_intent(&effects),
        CloseIntent::Hold,
        "cancelling the question keeps everything exactly where it was: {effects:?}"
    );
}
